//! The desktop side, on a private session bus: another app's "show in
//! folder" (`org.freedesktop.FileManager1.ShowItems`, sent with `gdbus`
//! like a browser would) selects the item in the running window, and a
//! second `fractale` hands its request over and exits.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use fractale::model::dbus::{claim, Claim};
use fractale::model::remote::Show;
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

#[test]
fn show_items_and_a_second_launch_reach_the_window() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let s = Scratch::new("dbus");
    s.file("first/a.txt", 3);
    let second = s.dir("second");
    let wanted = s.file("second/wanted name.txt", 3);
    s.file("second/other.txt", 3);

    let first = Show { folder: s.0.join("first"), select: Vec::new() };
    let remote = match claim(Some(&bus.address), &first).unwrap() {
        Claim::Primary(r) => r,
        Claim::Forwarded => panic!("nobody else is running"),
    };
    let mut h = window(&s.0.join("first"));
    h.state_mut().attach_remote(remote);
    listed(&mut h);

    // Another app's "show in folder".
    let uri = format!("file://{}", wanted.display()).replace(' ', "%20");
    let out = Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.freedesktop.FileManager1", "--object-path", "/org/freedesktop/FileManager1", "--method", "org.freedesktop.FileManager1.ShowItems"])
        .arg(format!("['{uri}']"))
        .arg("")
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .output();
    match out {
        Ok(o) => assert!(o.status.success(), "gdbus: {}", String::from_utf8_lossy(&o.stderr)),
        Err(_) => {
            eprintln!("no gdbus: ShowItems not checked");
        }
    }
    if s.0.join("second").exists() && Command::new("gdbus").arg("help").output().is_ok() {
        until(&mut h, Duration::from_secs(10), "ShowItems", |w| w.explorer.is_current() && w.explorer.folder == second && w.explorer.selection.is_selected(std::ffi::OsStr::new("wanted name.txt")));
        assert_eq!(h.state().explorer.selection.items.len(), 1);
    }

    // A second launch: hands over and exits, no window.
    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_fractale")).arg("--select").arg(s.0.join("first/a.txt")).env("DBUS_SESSION_BUS_ADDRESS", &bus.address).env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY").status().unwrap();
    assert!(status.success());
    eprintln!("second launch handed over and exited in {:?}", start.elapsed());
    assert!(start.elapsed() < Duration::from_secs(5));
    until(&mut h, Duration::from_secs(10), "the second launch's request", |w| w.explorer.is_current() && w.explorer.folder == s.0.join("first") && w.explorer.selection.is_selected(std::ffi::OsStr::new("a.txt")));
}
