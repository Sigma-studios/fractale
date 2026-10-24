use std::path::PathBuf;

const HELP: &str = "fractale — a file explorer for finding what uses disk space

USAGE: fractale [OPTIONS] [PATH]

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
            _ => path = Some(PathBuf::from(a)),
        }
    }
    let start = fractale::ui::Start::resolve(path, select, demo);
    if let Err(e) = fractale::ui::run(start) {
        eprintln!("fractale: {e}");
        std::process::exit(1);
    }
}

fn usage_error(msg: &str) -> ! {
    eprintln!("fractale: {msg}\n\n{HELP}");
    std::process::exit(2);
}
