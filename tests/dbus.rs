//! The desktop side, on a private session bus: another app's "show in
//! folder" (`org.freedesktop.FileManager1.ShowItems`, sent with `gdbus`
//! like a browser would) opens a new window, or shows in this one when the
//! bus started Fractale for it.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use fractale::model::dbus::serve;
use fractale::model::remote::{NewWindow, Show};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A dbus-daemon of our own, killed on drop.
struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        let mut child = Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address=1"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
        Some(Bus { child, address: line.trim().to_string() })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn show_items(bus: &Bus, files: &[&std::path::Path], startup_id: &str) -> bool {
    let uris: Vec<String> = files.iter().map(|f| format!("'file://{}'", f.display()).replace(' ', "%20")).collect();
    let out = Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.freedesktop.FileManager1", "--object-path", "/org/freedesktop/FileManager1", "--method", "org.freedesktop.FileManager1.ShowItems"])
        .arg(format!("[{}]", uris.join(", ")))
        .arg(startup_id)
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .output();
    match out {
        Ok(o) => {
            assert!(o.status.success(), "gdbus: {}", String::from_utf8_lossy(&o.stderr));
            true
        }
        Err(_) => {
            eprintln!("no gdbus: ShowItems not checked");
            false
        }
    }
}

#[test]
fn show_items_opens_new_windows() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let s = Scratch::new("dbus");
    let first = s.dir("first");
    s.file("first/a.txt", 3);
    let wanted = s.file("second/wanted name.txt", 3);
    let also = s.file("second/also.txt", 3);
    let third = s.file("third/c.txt", 3);

    let remote = serve(Some(&bus.address), false).unwrap();
    let mut h = window(&first);
    h.state_mut().attach_remote(remote);
    listed(&mut h);

    if !show_items(&bus, &[&wanted, &third, &also], "token-1") {
        return;
    }
    until(&mut h, Duration::from_secs(10), "ShowItems", |w| w.new_windows.len() == 2);
    let w = h.state();
    assert_eq!(
        w.new_windows,
        vec![
            NewWindow { show: Show { folder: s.0.join("second"), select: vec!["wanted name.txt".into(), "also.txt".into()] }, token: Some("token-1".into()) },
            NewWindow { show: Show { folder: s.0.join("third"), select: vec!["c.txt".into()] }, token: None },
        ]
    );
    // This window stays where it was.
    assert_eq!(w.explorer.folder, first);
}

#[test]
fn started_by_the_bus_shows_the_first_request_itself() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let s = Scratch::new("dbus-activated");
    let home = s.dir("home");
    let second = s.dir("second");
    let wanted = s.file("second/wanted.txt", 3);
    let later = s.file("home/later.txt", 3);

    let remote = serve(Some(&bus.address), true).unwrap();
    let mut h = window(&home);
    h.state_mut().attach_remote(remote);
    listed(&mut h);

    if !show_items(&bus, &[&wanted], "") {
        return;
    }
    until(&mut h, Duration::from_secs(10), "ShowItems", |w| w.explorer.is_current() && w.explorer.folder == second && w.explorer.selection.is_selected(std::ffi::OsStr::new("wanted.txt")));
    assert!(h.state().new_windows.is_empty());

    // The next one gets its own window.
    show_items(&bus, &[&later], "");
    until(&mut h, Duration::from_secs(10), "second ShowItems", |w| !w.new_windows.is_empty());
    assert_eq!(h.state().new_windows[0].show, Show { folder: home, select: vec!["later.txt".into()] });
    assert_eq!(h.state().explorer.folder, second);
}
