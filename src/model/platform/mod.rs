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

/// Lower the calling thread's CPU and I/O priority, for background work
/// (as Sentinelle does): nice 19 and the idle I/O class.
pub fn deprioritize_current_thread() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on the calling thread; failures are harmless.
    unsafe {
        // PRIO_PROCESS with id 0 applies to the calling thread on Linux.
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
    #[cfg(not(target_os = "linux"))]
    // SAFETY: setpriority on the calling process; failure is harmless.
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG);
    }
}
