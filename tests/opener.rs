//! Opening files: programs ask before running, "Open with" lists the apps
//! for the file's type. Linux only (macOS has no shared-mime-info); nothing
//! is launched.
#![cfg(target_os = "linux")]

mod common;

use common::Scratch;
use fractale::model::opener::{Opener, Reply, Req};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn programs_ask_and_open_with_lists_apps() {
    let s = Scratch::new("opener");
    let script = s.0.join("tool.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let o = Opener::spawn(Arc::new(|| {}));
    o.send(Req::Open(vec![script.clone()]));
    match o.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::AskRun(p) => assert_eq!(p, script),
        _ => panic!("a program must ask before running"),
    }
    let text = s.file("notes.txt", 10);
    o.send(Req::AppsFor(vec![text.clone()]));
    match o.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::Apps { files, mime, apps } => {
            assert_eq!(files, vec![text]);
            assert_eq!(mime, "text/plain");
            eprintln!("apps for text/plain: {:?}", apps.iter().map(|a| &a.name).collect::<Vec<_>>());
            assert!(apps.iter().filter(|a| a.default).count() <= 1);
        }
        _ => panic!("expected the app list"),
    }
}
