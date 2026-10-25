//! The window. Reads the model's state, sends it commands.

mod files;
pub mod folders;
pub mod icons;
pub mod ops;
pub mod previews;
mod search;
mod status;
mod text;
pub mod theme;
pub mod topbar;
mod widgets;

use crate::model::explorer::Explorer;
use crate::model::view::Pane;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use theme::*;

/// Where the window starts.
pub struct Start {
    pub folder: PathBuf,
    pub select: Option<PathBuf>,
    /// Where sizes come from (répertoire's socket, retry period).
    pub sources: crate::model::explorer::Sources,
}

impl Start {
    pub fn at(folder: PathBuf) -> Start {
        Start { folder, select: None, sources: Default::default() }
    }
}

impl Start {
    /// `--select FILE` opens FILE's folder; `--demo` the fixtures; otherwise
    /// PATH, or the home folder.
    pub fn resolve(path: Option<PathBuf>, select: Option<PathBuf>, demo: bool) -> Start {
        if let Some(file) = select {
            let file = absolute(&file);
            let folder = file.parent().map(Path::to_path_buf).unwrap_or_else(|| "/".into());
            return Start { folder, select: Some(file), sources: Default::default() };
        }
        let folder = if demo {
            fixtures_dir()
        } else {
            path.or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| "/".into())
        };
        Start { folder: absolute(&folder), select: None, sources: Default::default() }
    }
}

/// An absolute, lexically clean path (`.` and `..` resolved without
/// following symlinks, so a symlinked folder keeps its own path).
pub fn absolute(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_else(|_| "/".into()).join(p) };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    out
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
        // Ctrl+C/X/V are Fractale's (files); text fields get them back in
        // `raw_input_hook`.
        clipboard_shortcuts: false,
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
    /// With egui's clipboard shortcuts off, a focused text field gets
    /// copy, cut and paste from here.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if !ctx.egui_wants_keyboard_input() {
            return;
        }
        for ev in raw.events.iter_mut() {
            if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev {
                if !modifiers.command_only() {
                    continue;
                }
                match key {
                    egui::Key::C => *ev = egui::Event::Copy,
                    egui::Key::X => *ev = egui::Event::Cut,
                    egui::Key::V => {
                        if let Some(t) = crate::model::platform::clipboard_text() {
                            *ev = egui::Event::Paste(t);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

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
    pub explorer: Explorer,
    /// The pane keyboard navigation applies to.
    pub focus: Pane,
    pub topbar: topbar::TopBar,
    /// Icon size of grid tiles (Ctrl+wheel), 64..=256.
    pub tile: f32,
    pub(crate) names: text::NameCache,
    pub scroll: [PaneScroll; 2],
    /// Files activated (opened) — the UI hands them to the platform.
    pub opened: Vec<PathBuf>,
    /// Off in tests: record what would be opened instead of launching apps.
    pub launch: bool,
    ctx: Arc<OnceLock<egui::Context>>,
    pub stats: FrameStats,
    drop_focus: bool,
    /// Top-level folder rows on screen last frame (sized first).
    pub visible_folders: Vec<std::ffi::OsString>,
    /// Scroll the search cursor into view next frame.
    pub search_reveal: bool,
    /// The query last sent, and when the field last changed (searches
    /// start 100 ms after typing stops).
    search_sent: (String, bool),
    pub search_edited: Option<Instant>,
    /// "Biggest folders here" replaces the folder rows.
    pub show_top: bool,
    pub top_for: Option<PathBuf>,
    pub previews: previews::Previews,
    pub ops: ops::OpsUi,
}

/// What a pane's scroll area looked like last frame, to scroll the cursor
/// into view without laying out every row.
#[derive(Clone, Copy, Debug, Default)]
pub struct PaneScroll {
    pub offset: f32,
    pub viewport: f32,
    pub row_height: f32,
    /// Items per row (grid).
    pub columns: usize,
    /// Scroll here next frame.
    pub target: Option<f32>,
}

impl Window {
    pub fn new(start: Start) -> Window {
        let ctx: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        let waker = ctx.clone();
        let thumbnails = start.sources.thumbnails.clone();
        let previews = previews::Previews::new(thumbnails, Arc::new(wake(&ctx)));
        let select = start.select.as_ref().and_then(|s| s.file_name()).map(|n| n.to_owned());
        let explorer = Explorer::with_sources(start.folder, select, start.sources, wake(&waker));
        Window {
            explorer,
            focus: Pane::Folders,
            topbar: topbar::TopBar::default(),
            tile: 96.0,
            names: text::NameCache::default(),
            scroll: [PaneScroll::default(); 2],
            opened: Vec::new(),
            launch: true,
            ctx,
            stats: FrameStats::default(),
            drop_focus: false,
            visible_folders: Vec::new(),
            search_reveal: false,
            search_sent: (String::new(), false),
            search_edited: None,
            show_top: false,
            top_for: None,
            previews,
            ops: ops::OpsUi::default(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let started = Instant::now();
        let _ = self.ctx.set(ui.ctx().clone());
        self.explorer.poll();
        self.ops_frame();
        self.debounce_search(ui.ctx());
        if self.explorer.search.is_some() {
            search::keys(ui, self);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.topbar.search.clear();
                self.explorer.close_search();
                self.search_sent = (String::new(), false);
            }
        } else {
            self.keys(ui);
        }

        egui::Panel::top("topbar").frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(12, 8))).show(ui, |ui| {
            topbar::show(ui, self);
        });
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(14, 6))).show(ui, |ui| {
            ops::jobs_strip(ui, self);
            status::show(ui, self);
        });
        if self.explorer.search.is_some() {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(0, 8))).show(ui, |ui| {
                search::show(ui, self);
            });
        } else {
            egui::Panel::left("folders")
                .resizable(true)
                .default_size(460.0)
                .min_size(260.0)
                .frame(egui::Frame::new().fill(SURFACE).inner_margin(egui::Margin::symmetric(0, 8)))
                .show(ui, |ui| folders::show(ui, self));
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(0, 8))).show(ui, |ui| {
                files::show(ui, self);
            });
        }
        self.explorer.reveal_cursor = false;
        self.previews.end_frame(ui.ctx());
        ops::dialogs(ui.ctx(), self);
        // Fractale moves its own cursor with the keyboard; egui's focus
        // (which Tab and arrows would move between buttons) is only for
        // text fields.
        if !ui.ctx().egui_wants_keyboard_input() || std::mem::take(&mut self.drop_focus) {
            ui.ctx().memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        for path in std::mem::take(&mut self.opened) {
            if self.launch {
                crate::model::platform::open_default(&path);
            } else {
                self.opened.push(path);
                break;
            }
        }
        self.stats.record(started.elapsed());
    }

    /// Start the search 100 ms after the field stops changing.
    fn debounce_search(&mut self, ctx: &egui::Context) {
        let wanted = (self.topbar.search.clone(), self.topbar.everywhere);
        if wanted == self.search_sent {
            return;
        }
        let since = self.search_edited.map_or(Duration::MAX, |t| t.elapsed());
        if since >= Duration::from_millis(100) {
            self.explorer.set_search(&wanted.0, wanted.1);
            self.search_sent = wanted;
        } else {
            ctx.request_repaint_after(Duration::from_millis(100) - since);
        }
    }

    pub(crate) fn pane_index(pane: Pane) -> usize {
        match pane {
            Pane::Folders => 0,
            Pane::Files => 1,
        }
    }

    /// Open the cursor's item: folders in Fractale, files with their app.
    pub fn activate(&mut self, pane: Pane, name: &std::ffi::OsStr) {
        if let crate::model::explorer::Activate::File(path) = self.explorer.activate(pane, name) {
            self.opened.push(path);
        }
    }

    fn keys(&mut self, ui: &egui::Ui) {
        use egui::{Event, Key};
        let editing = ui.ctx().egui_wants_keyboard_input();
        let events = ui.input(|i| i.events.clone());
        let pointer_back = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra1));
        let pointer_forward = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Extra2));
        if pointer_back {
            self.explorer.back();
        }
        if pointer_forward {
            self.explorer.forward();
        }
        for ev in events {
            match ev {
                Event::Key { key, pressed: true, modifiers: m, .. } => {
                    let cmd = m.command;
                    // Shortcuts that work even while typing in a field.
                    match key {
                        Key::L if cmd => {
                            self.topbar.edit_path(&self.explorer.folder);
                            continue;
                        }
                        Key::F if cmd => {
                            self.topbar.focus_search = true;
                            continue;
                        }
                        Key::Escape if editing => {
                            self.topbar.cancel();
                            continue;
                        }
                        _ => {}
                    }
                    if editing {
                        continue;
                    }
                    if self.ops_key(ui.ctx(), key, m) {
                        continue;
                    }
                    let pane = self.focus;
                    let columns = self.scroll[Self::pane_index(pane)].columns.max(1) as i64;
                    let page = {
                        let s = &self.scroll[Self::pane_index(pane)];
                        ((s.viewport / s.row_height.max(1.0)).floor() as i64).max(1) * if pane == Pane::Files { columns } else { 1 }
                    };
                    let vertical = if pane == Pane::Files { columns } else { 1 };
                    match key {
                        Key::ArrowDown if !m.alt => self.explorer.step(pane, vertical, m.shift),
                        Key::ArrowUp if m.alt => self.explorer.up(),
                        Key::ArrowUp => self.explorer.step(pane, -vertical, m.shift),
                        Key::ArrowLeft if m.alt => self.explorer.back(),
                        Key::ArrowRight if m.alt => self.explorer.forward(),
                        Key::ArrowRight | Key::ArrowLeft if pane == Pane::Folders => {
                            // Expand / collapse the folder under the cursor.
                            if let Some((Pane::Folders, name)) = self.explorer.selection.cursor.clone() {
                                let path = self.explorer.folder.join(&name);
                                if (key == Key::ArrowRight) != self.explorer.is_expanded(&path) {
                                    self.explorer.toggle_expanded(path);
                                }
                            }
                        }
                        Key::ArrowLeft if pane == Pane::Files && columns > 1 => self.explorer.step(pane, -1, m.shift),
                        Key::ArrowRight if pane == Pane::Files && columns > 1 => self.explorer.step(pane, 1, m.shift),
                        Key::Home => self.explorer.step(pane, i64::MIN, m.shift),
                        Key::End => self.explorer.step(pane, i64::MAX, m.shift),
                        Key::PageDown => self.explorer.step(pane, page, m.shift),
                        Key::PageUp => self.explorer.step(pane, -page, m.shift),
                        Key::Tab => {
                            // egui's own Tab navigation focused some widget
                            // this frame; Tab is ours.
                            self.drop_focus = true;
                            self.focus = pane.other();
                            if self.explorer.selection.cursor.as_ref().is_none_or(|(p, _)| *p != self.focus) {
                                self.explorer.step(self.focus, 0, false);
                            }
                        }
                        Key::Enter => {
                            if let Some((p, name)) = self.explorer.selection.cursor.clone() {
                                self.activate(p, &name);
                            }
                        }
                        Key::Backspace => self.explorer.up(),
                        Key::A if cmd => self.explorer.select_all(),
                        Key::H if cmd => self.explorer.toggle_hidden(),
                        Key::F5 => self.explorer.refresh(),
                        Key::Escape => self.explorer.selection.clear(),
                        _ => {}
                    }
                }
                Event::Text(t) if !editing && !t.trim().is_empty() => self.explorer.type_ahead(self.focus, &t),
                _ => {}
            }
        }
    }
}

/// Frame times, for the "never blocks a frame" target.
#[derive(Default)]
pub struct FrameStats {
    pub frames: u64,
    pub worst: Duration,
    window_worst: Duration,
    window_start: Option<Instant>,
}

impl FrameStats {
    fn record(&mut self, took: Duration) {
        self.frames += 1;
        self.worst = self.worst.max(took);
        self.window_worst = self.window_worst.max(took);
        let start = *self.window_start.get_or_insert_with(Instant::now);
        if std::env::var_os("FRACTALE_FRAME_STATS").is_some() && start.elapsed() >= Duration::from_secs(1) {
            eprintln!("fractale: worst UI frame in the last second: {:.1?}", self.window_worst);
            self.window_worst = Duration::ZERO;
            self.window_start = Some(Instant::now());
        }
    }
}

/// Repaint from a worker thread, once the window exists.
fn wake(ctx: &Arc<OnceLock<egui::Context>>) -> impl Fn() + Send + Sync + 'static {
    let ctx = ctx.clone();
    move || {
        if let Some(c) = ctx.get() {
            c.request_repaint();
        }
    }
}
