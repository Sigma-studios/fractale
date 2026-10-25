use std::path::PathBuf;

const HELP: &str = "fractale — a file explorer for finding what uses disk space

USAGE: fractale [OPTIONS] [PATH | URI]

A file opens its folder with it selected. With Fractale already running,
the request goes to that window.

OPTIONS:
    --select <FILE>   Open FILE's folder with FILE selected
    --demo            Open the test fixtures (built by scripts/fixtures.sh)
    -h, --help        Show this help";

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut select: Option<PathBuf> = None;
    let mut demo = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--select") => select = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage_error("--select needs a file"))),
            Some("--demo") => demo = true,
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
    if select.is_none() && path.as_ref().is_some_and(|p| p.exists() && !p.is_dir()) {
        select = path.take();
    }
    let start = fractale::ui::Start::resolve(path, select, demo);
    let remote = single_instance(&start);
    if let Err(e) = fractale::ui::run(start, remote) {
        eprintln!("fractale: {e}");
        std::process::exit(1);
    }
}

/// Hand the request to a running Fractale (and exit), or become the one
/// that takes requests. Without a session bus Fractale just runs.
#[cfg(target_os = "linux")]
fn single_instance(start: &fractale::ui::Start) -> Option<fractale::model::remote::Remote> {
    use fractale::model::dbus::{claim, Claim};
    let select = start.select.as_ref().and_then(|s| s.file_name()).map(|n| vec![n.to_owned()]).unwrap_or_default();
    let show = fractale::model::remote::Show { folder: start.folder.clone(), select };
    match claim(None, &show) {
        Ok(Claim::Forwarded) => std::process::exit(0),
        Ok(Claim::Primary(r)) => Some(r),
        Err(e) => {
            eprintln!("fractale: no session bus ({e}); running on its own");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn single_instance(_: &fractale::ui::Start) -> Option<fractale::model::remote::Remote> {
    None
}

fn usage_error(msg: &str) -> ! {
    eprintln!("fractale: {msg}\n\n{HELP}");
    std::process::exit(2);
}
