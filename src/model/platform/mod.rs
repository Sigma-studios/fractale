//! What differs between Linux (the target) and macOS (where Fractale is
//! developed). The macOS side only exists for development.

use std::path::Path;

/// Open a file with the desktop's default application, detached.
pub fn open_default(path: &Path) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    spawn_detached(std::process::Command::new(program).arg(path));
}

/// Start a program without waiting for it or keeping its output.
pub fn spawn_detached(cmd: &mut std::process::Command) {
    use std::process::Stdio;
    if let Ok(mut child) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        // Reap it in the background so it doesn't linger as a zombie.
        std::thread::spawn(move || child.wait());
    }
}
