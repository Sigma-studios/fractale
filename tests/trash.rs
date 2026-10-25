//! Moving to the trash. On Linux the trash is the freedesktop one, which
//! KDE, GNOME and `gio` share: the test points `XDG_DATA_HOME` at a
//! scratch folder (its own test binary, so nothing else sees the change),
//! checks the `.trashinfo` files against the spec and restores with `gio`
//! when it's installed. On macOS the real Trash is the only one, so that
//! check runs only when asked (`FRACTALE_TEST_MAC_TRASH=1`) and removes
//! what it put there.

mod common;

use common::Scratch;
use fractale::model::ops::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn trash(paths: Vec<std::path::PathBuf>) -> JobView {
    let j = Jobs::spawn(Arc::new(|| {}));
    let id = j.submit(Spec { kind: Kind::Trash, sources: paths, dest: None });
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30));
        let _ = j.rx.recv_timeout(Duration::from_millis(20));
        let v = j.view(id).unwrap();
        assert!(v.question.is_none(), "{:?}", v.question);
        if v.outcome.is_some() {
            return v;
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn freedesktop_trash() {
    let s = Scratch::new("trash");
    let data = s.dir("data");
    // SAFETY (not unsafe in edition 2021, but racy): the only test in this
    // binary, before any thread reads the environment.
    std::env::set_var("XDG_DATA_HOME", &data);
    let name = "a b%é.txt";
    let file = s.file(name, 10);
    let folder = s.dir("folder");
    std::fs::write(folder.join("inside"), "x").unwrap();
    let v = trash(vec![file.clone(), folder.clone()]);
    assert_eq!(v.outcome.unwrap().done, 2);
    assert!(!file.exists() && !folder.exists());
    let t = data.join("Trash");
    assert!(t.join("files").join(name).is_file());
    assert!(t.join("files/folder/inside").is_file());
    let info = std::fs::read_to_string(t.join("info").join(format!("{name}.trashinfo"))).unwrap();
    eprintln!("{info}");
    let mut lines = info.lines();
    assert_eq!(lines.next(), Some("[Trash Info]"));
    let path = info.lines().find_map(|l| l.strip_prefix("Path=")).unwrap();
    // Absolute, percent-encoded (the spec: like a URL's path).
    assert_eq!(percent_decode(path), file.to_string_lossy());
    assert!(!path.contains(' ') && path.contains("%25"), "{path}");
    let date = info.lines().find_map(|l| l.strip_prefix("DeletionDate=")).unwrap();
    assert_eq!(date.len(), 19, "YYYY-MM-DDThh:mm:ss: {date}");
    assert_eq!(&date[10..11], "T");

    // Restore the way desktop tools do (gio follows the same spec as KDE).
    // gio's trash:// lives in gvfs, which needs a session bus.
    let gio = |args: &[&str]| std::process::Command::new("dbus-run-session").arg("--").arg("gio").args(args).env("XDG_DATA_HOME", &data).stderr(std::process::Stdio::null()).output();
    let listed = gio(&["trash", "--list"]);
    match listed {
        Ok(o) if o.status.success() => {
            let list = String::from_utf8_lossy(&o.stdout).into_owned();
            eprintln!("gio trash --list:\n{list}");
            // gio prints non-ASCII bytes escaped (é as \xc3\xa9).
            let original = format!("{}/a b%\\xc3\\xa9.txt", s.0.display());
            assert!(list.contains(&original), "gio sees the original path {original}");
            let uri = list.lines().find(|l| l.contains("folder")).and_then(|l| l.split('\t').next()).unwrap().to_string();
            assert!(gio(&["trash", "--restore", &uri]).unwrap().status.success());
            assert!(folder.join("inside").is_file(), "restored by gio");
        }
        _ => eprintln!("no gio with gvfs and dbus-run-session: restore not checked"),
    }
}

#[cfg(target_os = "linux")]
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn macos_trash() {
    if std::env::var_os("FRACTALE_TEST_MAC_TRASH").is_none() {
        eprintln!("FRACTALE_TEST_MAC_TRASH not set: skipped (it uses the real Trash)");
        return;
    }
    let s = Scratch::new("trash");
    let name = format!("fractale-trash-test-{}.txt", std::process::id());
    let file = s.file(&name, 10);
    let v = trash(vec![file.clone()]);
    assert_eq!(v.outcome.unwrap().done, 1);
    assert!(!file.exists());
    let in_trash = std::path::Path::new(&std::env::var_os("HOME").unwrap()).join(".Trash").join(&name);
    let found = in_trash.exists();
    // Take back what we put there.
    let _ = std::fs::remove_file(&in_trash);
    assert!(found, "{} in the Trash", in_trash.display());
}
