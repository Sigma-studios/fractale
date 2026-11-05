use std::path::PathBuf;

const HELP: &str = "fractale — a file explorer for finding what uses disk space

USAGE: fractale [OPTIONS] [PATH | URI]

A file opens its folder with it selected. Every launch opens a window.

OPTIONS:
    --select <FILE>   Open FILE's folder with FILE selected (repeatable:
                      files in the same folder)
    --demo            Open the test fixtures (built by scripts/fixtures.sh)
    -h, --help        Show this help";

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut select: Vec<PathBuf> = Vec::new();
    let mut demo = false;
    // Started by the bus for a "show in folder" (see dist/).
    let mut activated = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--select") => select.push(args.next().map(PathBuf::from).unwrap_or_else(|| usage_error("--select needs a file"))),
            Some("--demo") => demo = true,
            Some("--activated") => activated = true,
            Some("-h" | "--help") => {
                println!("{HELP}");
                return;
            }
            Some(s) if s.starts_with('-') => usage_error(&format!("unknown option {s}")),
            Some(s) if s.starts_with("file://") => path = Some(fractale::model::remote::path_of_uri(s).unwrap_or_else(|| usage_error(&format!("bad URI {s}")))),
            _ => path = Some(PathBuf::from(a)),
        }
    }
    // A file given as the path: its folder, with it selected.
    if select.is_empty() && path.as_ref().is_some_and(|p| p.exists() && !p.is_dir()) {
        select.extend(path.take());
    }
    let start = fractale::ui::Start::resolve(path, select, demo);
    let remote = file_manager(activated);
    if let Err(e) = fractale::ui::run(start, remote) {
        eprintln!("fractale: {e}");
        std::process::exit(1);
    }
}

/// Take "show in folder" requests from other apps, when this Fractale gets
/// the desktop's file manager name. Without a session bus Fractale just runs.
#[cfg(target_os = "linux")]
fn file_manager(activated: bool) -> Option<fractale::model::remote::Remote> {
    match fractale::model::dbus::serve(None, activated) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("fractale: no session bus ({e}); \"show in folder\" from other apps won't reach it");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn file_manager(_: bool) -> Option<fractale::model::remote::Remote> {
    None
}

fn usage_error(msg: &str) -> ! {
    eprintln!("fractale: {msg}\n\n{HELP}");
    std::process::exit(2);
}
