//! The window. Reads the model's state, sends it commands.

pub mod theme;

use eframe::egui::{self, Layout, RichText};
use std::path::{Path, PathBuf};
use theme::*;

/// Where the window starts.
pub struct Start {
    pub folder: PathBuf,
    pub select: Option<PathBuf>,
}

impl Start {
    /// `--select FILE` opens FILE's folder; `--demo` the fixtures; otherwise
    /// PATH, or the home folder.
    pub fn resolve(path: Option<PathBuf>, select: Option<PathBuf>, demo: bool) -> Start {
        if let Some(file) = select {
            let folder = file.parent().map(Path::to_path_buf).unwrap_or_else(|| "/".into());
            return Start { folder, select: Some(file) };
        }
        let folder = if demo {
            fixtures_dir()
        } else {
            path.or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| "/".into())
        };
        Start { folder, select: None }
    }
}

/// `target/fixtures` of the source tree, built on demand.
pub fn fixtures_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::var_os("FRACTALE_FIXTURES").map(PathBuf::from).unwrap_or_else(|| root.join("target/fixtures"));
    if !dir.exists() {
        let status = std::process::Command::new(root.join("scripts/fixtures.sh")).arg(&dir).status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("fractale: couldn't build fixtures in {}", dir.display());
        }
    }
    dir
}

pub fn run(start: Start) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Fractale")
            .with_app_id("fractale")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([720.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "fractale",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App { window: Window::new(start), screenshot: std::env::var("FRACTALE_SCREENSHOT").ok().map(|p| (p, false)) }))
        }),
    )
}

struct App {
    window: Window,
    /// `FRACTALE_SCREENSHOT=out.ppm`: save the window after
    /// `FRACTALE_SCREENSHOT_DELAY` seconds (default 1) and quit.
    screenshot: Option<(String, bool)>,
}

impl App {
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        static DONE: AtomicBool = AtomicBool::new(false);
        let Some((path, asked)) = &mut self.screenshot else { return };
        if DONE.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let delay: f64 = std::env::var("FRACTALE_SCREENSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        if !*asked && ctx.input(|i| i.time) >= delay {
            *asked = true;
            let path = path.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img: Arc<egui::ColorImage>| {
                let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
                for px in &img.pixels {
                    out.extend_from_slice(&[px.r(), px.g(), px.b()]);
                }
                let _ = std::fs::write(&path, out);
                DONE.store(true, Ordering::Relaxed);
            })));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

impl eframe::App for App {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.window.ui(ui);
        let ctx = ui.ctx().clone();
        self.handle_screenshot(&ctx);
    }
}

/// Everything inside the window, separate from eframe so tests can drive it.
pub struct Window {
    pub folder: PathBuf,
}

impl Window {
    pub fn new(start: Start) -> Window {
        Window { folder: start.folder }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("topbar").frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(12, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.folder.display().to_string()).color(TEXT));
            });
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(12, 6))).show(ui, |ui| {
            ui.label(RichText::new("").color(MUTED));
        });
        egui::Panel::left("folders")
            .resizable(true)
            .default_size(460.0)
            .frame(egui::Frame::new().fill(SURFACE).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| {
                ui.label(eyebrow("Folders", FAINT));
                ui.allocate_space(ui.available_size());
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(10))).show(ui, |ui| {
            ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                ui.label(eyebrow("Files", FAINT));
            });
        });
    }
}
