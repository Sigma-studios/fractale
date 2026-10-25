//! File operations against temporary directories: copies keep what they
//! should, moves across filesystems remove sources only once copied,
//! conflicts and errors ask, cancel leaves no partial file.

mod common;

use common::Scratch;
use fractale::model::ops::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

fn jobs() -> Jobs {
    Jobs::spawn(Arc::new(|| {}))
}

enum Reply {
    Conflict(Resolution, bool),
    Error(ErrorChoice),
}

/// Run a job to the end, answering its questions with `policy`; the
/// questions asked, in order.
fn run(j: &Jobs, spec: Spec, mut policy: impl FnMut(&Question) -> Reply) -> (JobView, Vec<Question>) {
    let id = j.submit(spec);
    let start = Instant::now();
    let mut asked = Vec::new();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "job didn't finish");
        let _ = j.rx.recv_timeout(Duration::from_millis(20));
        let v = j.view(id).unwrap();
        if v.outcome.is_some() {
            return (v, asked);
        }
        if let Some(q) = v.question.clone() {
            match policy(&q) {
                Reply::Conflict(r, all) => j.resolve(id, r, all),
                Reply::Error(c) => j.error_choice(id, c),
            }
            asked.push(q);
        }
    }
}

fn no_questions(q: &Question) -> Reply {
    panic!("unexpected question: {q:?}")
}

fn copy(sources: Vec<PathBuf>, dest: &Path) -> Spec {
    Spec { kind: Kind::Copy, sources, dest: Some(dest.to_path_buf()) }
}

fn mv(sources: Vec<PathBuf>, dest: &Path) -> Spec {
    Spec { kind: Kind::Move, sources, dest: Some(dest.to_path_buf()) }
}

fn set_mtime(p: &Path, secs_ago: u64) {
    let t = SystemTime::now() - Duration::from_secs(secs_ago);
    std::fs::File::open(p).unwrap().set_times(std::fs::FileTimes::new().set_modified(t)).unwrap();
}

/// A small tree with the things copies must keep.
fn tree(root: &Path) -> PathBuf {
    let t = root.join("tree");
    std::fs::create_dir_all(t.join("sub/deeper")).unwrap();
    std::fs::write(t.join("a.txt"), "alpha").unwrap();
    std::fs::write(t.join("sub/b.bin"), vec![7u8; 300_000]).unwrap();
    std::fs::write(t.join("sub/deeper/c.txt"), "gamma").unwrap();
    std::fs::write(t.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(t.join("run.sh"), std::fs::Permissions::from_mode(0o750)).unwrap();
    std::os::unix::fs::symlink("sub/deeper/c.txt", t.join("link")).unwrap();
    std::os::unix::fs::symlink("nowhere", t.join("dangling")).unwrap();
    set_mtime(&t.join("a.txt"), 86_400 * 30);
    set_mtime(&t.join("sub/deeper"), 86_400 * 3);
    std::fs::set_permissions(t.join("sub/deeper"), std::fs::Permissions::from_mode(0o555)).unwrap();
    t
}

/// Same names, contents, permissions, mtimes and link targets.
fn same_tree(a: &Path, b: &Path) {
    let (ma, mb) = (std::fs::symlink_metadata(a).unwrap(), std::fs::symlink_metadata(b).unwrap());
    assert_eq!(ma.file_type(), mb.file_type(), "{}", b.display());
    assert_eq!(ma.mode() & 0o7777, mb.mode() & 0o7777, "permissions of {}", b.display());
    if ma.file_type().is_symlink() {
        assert_eq!(std::fs::read_link(a).unwrap(), std::fs::read_link(b).unwrap());
        return;
    }
    assert_eq!(ma.mtime(), mb.mtime(), "mtime of {}", b.display());
    if ma.is_dir() {
        let names = |p: &Path| {
            let mut v: Vec<_> = std::fs::read_dir(p).unwrap().map(|e| e.unwrap().file_name()).collect();
            v.sort();
            v
        };
        assert_eq!(names(a), names(b), "contents of {}", b.display());
        for n in names(a) {
            same_tree(&a.join(&n), &b.join(&n));
        }
    } else {
        assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap(), "{}", b.display());
    }
}

fn no_part_files(p: &Path) {
    for e in std::fs::read_dir(p).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        assert!(!n.ends_with(".fractale-part"), "partial file left: {}", e.path().display());
        if e.file_type().unwrap().is_dir() {
            no_part_files(&e.path());
        }
    }
}

fn is_root() -> bool {
    // SAFETY: no arguments, no side effects.
    unsafe { libc::geteuid() == 0 }
}

#[test]
fn copy_keeps_contents_permissions_times_and_links() {
    let s = Scratch::new("ops-copy");
    let t = tree(&s.0);
    let dest = s.dir("dest");
    let j = jobs();
    let (v, _) = run(&j, copy(vec![t.clone()], &dest), no_questions);
    let o = v.outcome.unwrap();
    assert_eq!((o.skipped, o.cancelled), (0, false));
    assert_eq!(o.created, vec![dest.join("tree")]);
    same_tree(&t, &dest.join("tree"));
    assert_eq!(v.progress.items_done, v.progress.items_total);
    assert_eq!(v.progress.bytes_done, v.progress.bytes_total);
    no_part_files(&dest);
    // Into the same folder: a copy next to it.
    let (v, _) = run(&j, copy(vec![t.join("a.txt")], &t), no_questions);
    assert_eq!(v.outcome.unwrap().created, vec![t.join("a (2).txt")]);
    std::fs::set_permissions(t.join("sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(dest.join("tree/sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn move_on_one_filesystem_is_a_rename() {
    let s = Scratch::new("ops-move");
    let t = tree(&s.0);
    let ino = std::fs::metadata(t.join("sub/b.bin")).unwrap().ino();
    let dest = s.dir("dest");
    let (v, _) = run(&jobs(), mv(vec![t.clone()], &dest), no_questions);
    assert_eq!(v.outcome.unwrap().done, 1);
    assert!(!t.exists());
    assert_eq!(std::fs::metadata(dest.join("tree/sub/b.bin")).unwrap().ino(), ino, "same file, not a copy");
    std::fs::set_permissions(dest.join("tree/sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_folder_cant_go_into_itself() {
    let s = Scratch::new("ops-itself");
    let t = tree(&s.0);
    let (v, asked) = run(&jobs(), copy(vec![t.clone()], &t.join("sub")), |_| Reply::Error(ErrorChoice::Skip));
    assert_eq!(v.outcome.unwrap().skipped, 1);
    assert!(matches!(&asked[..], [Question::Error { .. }]));
    assert!(!t.join("sub/tree").exists());
    std::fs::set_permissions(t.join("sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn conflicts_replace_skip_keep_both_and_apply_to_all() {
    let s = Scratch::new("ops-conflicts");
    let src = s.dir("src");
    let dest = s.dir("dest");
    for n in ["a", "b", "c"] {
        std::fs::write(src.join(format!("{n}.txt")), format!("new {n}")).unwrap();
        std::fs::write(dest.join(format!("{n}.txt")), format!("old {n}")).unwrap();
    }
    std::fs::write(src.join("d.txt"), "only new").unwrap();
    let sources: Vec<PathBuf> = ["a", "b", "c", "d"].iter().map(|n| src.join(format!("{n}.txt"))).collect();
    let mut answers = vec![Reply::Conflict(Resolution::Replace, false), Reply::Conflict(Resolution::Skip, false), Reply::Conflict(Resolution::KeepBoth, false)].into_iter();
    let (v, asked) = run(&jobs(), copy(sources.clone(), &dest), |_| answers.next().unwrap());
    assert_eq!(asked.len(), 3);
    match &asked[0] {
        Question::Conflict { source, target, source_brief, target_brief } => {
            assert_eq!((source, target), (&src.join("a.txt"), &dest.join("a.txt")));
            assert_eq!((source_brief.size, target_brief.size), (5, 5));
        }
        q => panic!("{q:?}"),
    }
    let read = |n: &str| std::fs::read_to_string(dest.join(n)).unwrap();
    assert_eq!(read("a.txt"), "new a");
    assert_eq!(read("b.txt"), "old b");
    assert_eq!((read("c.txt").as_str(), read("c (2).txt").as_str()), ("old c", "new c"));
    assert_eq!(read("d.txt"), "only new");
    assert_eq!(v.outcome.unwrap().skipped, 1);

    // "Apply to all": asked once.
    let (_, asked) = run(&jobs(), copy(sources, &dest), |_| Reply::Conflict(Resolution::Skip, true));
    assert_eq!(asked.len(), 1);

    // Folders merge; only files inside conflict.
    let (fa, fb) = (s.dir("fa/x"), s.dir("fb/x"));
    std::fs::write(fa.join("same.txt"), "from a").unwrap();
    std::fs::write(fa.join("only-a.txt"), "a").unwrap();
    std::fs::write(fb.join("same.txt"), "from b").unwrap();
    let (_, asked) = run(&jobs(), copy(vec![fa.clone()], &s.0.join("fb")), |_| Reply::Conflict(Resolution::Replace, false));
    assert_eq!(asked.len(), 1);
    assert_eq!(std::fs::read_to_string(fb.join("same.txt")).unwrap(), "from a");
    assert!(fb.join("only-a.txt").exists());
}

#[test]
fn cancelling_a_copy_leaves_no_partial_file() {
    let s = Scratch::new("ops-cancel");
    let big = s.0.join("big.bin");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&big).unwrap();
        let chunk: Vec<u8> = (0..1 << 20).map(|i| (i * 31) as u8).collect();
        for _ in 0..512 {
            f.write_all(&chunk).unwrap();
        }
    }
    let dest = s.dir("dest");
    let j = jobs();
    let id = j.submit(copy(vec![big.clone()], &dest));
    let start = Instant::now();
    let mut cancelled_at = None;
    loop {
        assert!(start.elapsed() < Duration::from_secs(60));
        let _ = j.rx.recv_timeout(Duration::from_millis(5));
        let v = j.view(id).unwrap();
        if cancelled_at.is_none() && v.progress.bytes_done > 0 {
            cancelled_at = Some(v.progress.bytes_done);
            j.cancel(id);
        }
        if let Some(o) = v.outcome {
            eprintln!("cancelled after {:?} bytes; outcome {o:?}", cancelled_at);
            if o.cancelled {
                assert!(!dest.join("big.bin").exists(), "no target");
            } else {
                // Finished before the cancel landed: then it's whole.
                assert_eq!(std::fs::metadata(dest.join("big.bin")).unwrap().len(), 512 << 20);
            }
            break;
        }
    }
    no_part_files(&dest);
}

#[test]
fn permission_errors_ask_and_retry_or_skip() {
    if is_root() {
        eprintln!("root reads everything: skipped");
        return;
    }
    let s = Scratch::new("ops-perms");
    let src = s.dir("src");
    for n in ["a", "b", "c"] {
        std::fs::write(src.join(n), n).unwrap();
    }
    std::fs::set_permissions(src.join("b"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let dest = s.dir("dest");
    let sources: Vec<PathBuf> = ["a", "b", "c"].iter().map(|n| src.join(n)).collect();

    // Skip: the others are copied.
    let (v, asked) = run(&jobs(), copy(sources.clone(), &dest), |_| Reply::Error(ErrorChoice::Skip));
    let o = v.outcome.unwrap();
    assert_eq!((o.done, o.skipped), (2, 1));
    match &asked[..] {
        [Question::Error { path, error, .. }] => {
            assert_eq!(path, &src.join("b"));
            assert!(error.to_lowercase().contains("permission"), "{error}");
        }
        q => panic!("{q:?}"),
    }
    assert!(!dest.join("b").exists());
    no_part_files(&dest);

    // Retry after fixing it: copied.
    let b = src.join("b");
    let (v, _) = run(&jobs(), copy(vec![b.clone()], &dest), |_| {
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o644)).unwrap();
        Reply::Error(ErrorChoice::Retry)
    });
    assert_eq!(v.outcome.unwrap().done, 1);
    assert_eq!(std::fs::read_to_string(dest.join("b")).unwrap(), "b");

    // An unwritable destination: cancel stops the job.
    let ro = s.dir("ro");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (v, asked) = run(&jobs(), copy(sources, &ro), |_| Reply::Error(ErrorChoice::Cancel));
    assert!(v.outcome.unwrap().cancelled);
    assert_eq!(asked.len(), 1);
}

#[test]
fn delete_removes_everything_or_says_what_stayed() {
    let s = Scratch::new("ops-delete");
    let t = tree(&s.0);
    std::fs::set_permissions(t.join("sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let other = s.file("other.txt", 10);
    let (v, _) = run(&jobs(), Spec { kind: Kind::Delete, sources: vec![t.clone(), other.clone()], dest: None }, no_questions);
    assert_eq!(v.outcome.unwrap().done, 2);
    assert!(!t.exists() && !other.exists());
    assert_eq!(v.progress.items_done, v.progress.items_total);

    if !is_root() {
        let locked = s.dir("locked/inner");
        std::fs::write(locked.join("f"), "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let (v, asked) = run(&jobs(), Spec { kind: Kind::Delete, sources: vec![s.0.join("locked")], dest: None }, |_| Reply::Error(ErrorChoice::Skip));
        assert_eq!(v.outcome.unwrap().skipped, 1);
        assert!(matches!(&asked[0], Question::Error { path, .. } if path == &locked.join("f")));
        assert!(locked.join("f").exists());
    }
}

#[test]
fn quick_operations() {
    let s = Scratch::new("ops-quick");
    let j = jobs();
    let ask = |q: Quick| {
        j.quick(q);
        loop {
            if let Msg::Quick(d) = j.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                return d;
            }
        }
    };
    let a = s.file("a.txt", 3);
    s.file("b.txt", 3);
    assert!(matches!(ask(Quick::Rename { path: a.clone(), name: "b.txt".into() }), QuickDone::Failed { error, .. } if error.contains("already exists")));
    assert!(matches!(ask(Quick::Rename { path: a.clone(), name: "x/y".into() }), QuickDone::Failed { .. }));
    assert!(matches!(ask(Quick::Rename { path: a.clone(), name: "c.txt".into() }), QuickDone::Renamed { to, .. } if to == s.0.join("c.txt")));
    assert!(matches!(ask(Quick::NewFolder { parent: s.0.clone() }), QuickDone::Created(p) if p == s.0.join("New folder")));
    assert!(matches!(ask(Quick::NewFolder { parent: s.0.clone() }), QuickDone::Created(p) if p == s.0.join("New folder (2)")));
    match ask(Quick::Count { paths: vec![s.0.clone()] }) {
        QuickDone::Counted { items, .. } => assert_eq!(items, 5),
        d => panic!("{d:?}"),
    }
}

/// A second filesystem: a disk image on macOS (no root needed), a
/// loopback ext4 on Linux (root only; `scripts/vm.sh test` runs as root
/// too).
struct OtherFs {
    mount: PathBuf,
    image: PathBuf,
}

impl OtherFs {
    fn new(s: &Scratch) -> Option<OtherFs> {
        let image = s.0.join("fs.img");
        let mount = s.dir("mnt");
        let ok = |c: &mut std::process::Command| c.stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        if cfg!(target_os = "macos") {
            let dmg = image.with_extension("dmg");
            let made = ok(std::process::Command::new("hdiutil").args(["create", "-quiet", "-size", "64m", "-fs", "HFS+", "-volname", "fractale-test"]).arg(&dmg))
                && ok(std::process::Command::new("hdiutil").args(["attach", "-quiet", "-nobrowse", "-mountpoint"]).arg(&mount).arg(&dmg));
            return made.then_some(OtherFs { mount, image: dmg });
        }
        if !is_root() {
            return None;
        }
        let made = ok(std::process::Command::new("truncate").args(["-s", "64M"]).arg(&image))
            && ok(std::process::Command::new("mkfs.ext4").args(["-q", "-F"]).arg(&image))
            && ok(std::process::Command::new("mount").args(["-o", "loop"]).arg(&image).arg(&mount));
        made.then_some(OtherFs { mount, image })
    }
}

impl Drop for OtherFs {
    fn drop(&mut self) {
        if cfg!(target_os = "macos") {
            let _ = std::process::Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&self.mount).status();
        } else {
            let _ = std::process::Command::new("umount").arg(&self.mount).status();
        }
        let _ = std::fs::remove_file(&self.image);
    }
}

#[test]
fn copy_and_move_across_filesystems() {
    let s = Scratch::new("ops-xfs");
    let Some(other) = OtherFs::new(&s) else {
        eprintln!("no second filesystem (needs root on Linux): skipped");
        return;
    };
    let t = tree(&s.0);
    // A read-only folder can't be emptied, so the move would rightly ask.
    std::fs::set_permissions(t.join("sub/deeper"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let reference = s.0.join("reference");
    let (v, _) = run(&jobs(), copy(vec![t.clone()], &s.0), |_| Reply::Conflict(Resolution::KeepBoth, false));
    std::fs::rename(&v.outcome.unwrap().created[0], &reference).unwrap();
    let dev = |p: &Path| std::fs::metadata(p).unwrap().dev();
    assert_ne!(dev(&s.0), dev(&other.mount));

    // Copy there.
    let (v, _) = run(&jobs(), copy(vec![t.clone()], &other.mount), no_questions);
    assert_eq!(v.outcome.unwrap().skipped, 0);
    same_tree(&t, &other.mount.join("tree"));
    no_part_files(&other.mount);

    // Move back over a cleared spot: copied, then the sources removed.
    std::fs::remove_dir_all(&t).unwrap();
    let start = Instant::now();
    let (v, _) = run(&jobs(), mv(vec![other.mount.join("tree")], &s.0), no_questions);
    eprintln!("moved across filesystems in {:?}", start.elapsed());
    let o = v.outcome.unwrap();
    assert_eq!((o.skipped, o.cancelled), (0, false));
    assert!(!other.mount.join("tree").exists(), "sources removed");
    same_tree(&reference, &t);
    no_part_files(&s.0);
}
