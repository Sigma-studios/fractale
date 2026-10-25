//! Shared helpers for UI tests.
#![allow(dead_code)]

use egui_kittest::{Harness, HarnessBuilder};

/// A harness with Fractale's theme (bundled fonts only, so snapshots don't
/// depend on the machine).
pub fn builder<S>() -> HarnessBuilder<S> {
    Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_context_setup(fractale::ui::theme::install_for_tests)
}

use fractale::ui::{Start, Window};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A fresh directory for one test, removed by `Scratch::drop`.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = std::env::temp_dir().canonicalize().unwrap();
        let d = base.join(format!("fractale-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }

    pub fn file(&self, rel: &str, size: usize) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![b'x'; size]).unwrap();
        p
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        fn unlock(p: &Path) {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        unlock(&e.path());
                    }
                }
            }
        }
        unlock(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub type H<'a> = egui_kittest::Harness<'a, Window>;

/// A harness around a window opened on `folder`, with app launching off
/// and no répertoire (a socket nobody listens on).
pub fn window(folder: &Path) -> H<'static> {
    window_with(folder, fractale::model::explorer::Sources { socket: "/nonexistent/fractale-test.sock".into(), retry: Duration::from_secs(3600), thumbnails: thumbnails() })
}

pub fn window_with(folder: &Path, sources: fractale::model::explorer::Sources) -> H<'static> {
    let mut w = Window::new(Start { folder: folder.to_path_buf(), select: None, sources });
    w.launch = false;
    builder().build_ui_state(|ui, w: &mut Window| w.ui(ui), w)
}

/// Step frames (in real time, so worker threads make progress) until `cond`
/// holds. Panics after `timeout`.
pub fn until(h: &mut H, timeout: Duration, what: &str, cond: impl Fn(&Window) -> bool) -> Duration {
    let start = Instant::now();
    loop {
        h.step();
        if cond(h.state()) {
            return start.elapsed();
        }
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait for the listing of the folder asked for.
pub fn listed(h: &mut H) {
    until(h, Duration::from_secs(10), "the listing", |w| w.explorer.is_current());
    h.run_steps(2);
}

/// The thumbnail cache tests use, inside the build directory (never the
/// user's).
pub fn thumbnails() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("thumbnails")
}

/// Click a folder row on its name (its middle may hold the peek button).
pub fn click_row(h: &mut H, label: &str) {
    use egui_kittest::kittest::Queryable;
    let r = h.get_by_label(label).rect();
    let at = egui::pos2(r.left() + 60.0, r.center().y);
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
}
