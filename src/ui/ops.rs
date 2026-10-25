//! File operations in the window: the context menus and shortcuts, rename
//! in place, the dialogs (delete, conflicts, errors), the jobs strip, and
//! dragging items onto folder rows.

use super::theme::*;
use super::Window;
use crate::model::ops::{ErrorChoice, JobView, Kind, Question, Resolution};
use crate::model::view::Pane;
use eframe::egui::{self, vec2, Key, RichText};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct OpsUi {
    pub rename: Option<Rename>,
    /// Waiting for "Delete permanently?".
    pub confirm_delete: Option<Vec<PathBuf>>,
    /// "Apply to all" in the conflict dialog.
    apply_all: bool,
    /// "Open with": asked for, then the apps that can.
    pub open_with: Option<OpenWith>,
    /// A program the user tried to open: run it?
    pub ask_run: Option<PathBuf>,
}

pub struct OpenWith {
    pub files: Vec<PathBuf>,
    pub apps: Option<Vec<crate::model::opener::AppChoice>>,
    pub mime: String,
    pub command: String,
}

pub struct Rename {
    /// Which pane the item is in (the files pane may show a peeked folder).
    pub pane: Pane,
    pub name: OsString,
    pub text: String,
    /// The first frame: focus the field and select the name without its
    /// extension.
    fresh: bool,
}

/// Files dragged inside Fractale.
#[derive(Clone, Debug)]
pub struct Dragged {
    pub paths: Vec<PathBuf>,
    /// Their filesystems, to tell a move from a copy.
    pub devs: Vec<u64>,
}

impl Window {
    pub fn start_rename(&mut self) {
        let Some((pane, name)) = self.explorer.selection.cursor.clone() else { return };
        if self.explorer.find_in(pane, &name).is_none() {
            return;
        }
        let text = name.to_string_lossy().into_owned();
        self.ops.rename = Some(Rename { pane, name, text, fresh: true });
    }

    fn selected_or_cursor(&self) -> Vec<PathBuf> {
        let mut v = self.explorer.selected_paths();
        if v.is_empty() {
            if let Some((p, n)) = &self.explorer.selection.cursor {
                v.push(self.explorer.path_in(*p, n));
            }
        }
        v
    }

    pub fn trash_selection(&mut self) {
        let paths = self.selected_or_cursor();
        self.explorer.trash(paths);
    }

    pub fn ask_delete(&mut self) {
        let paths = self.selected_or_cursor();
        if !paths.is_empty() {
            self.explorer.count(paths.clone());
            self.ops.confirm_delete = Some(paths);
        }
    }

    pub fn copy_paths_as_text(&mut self, ctx: &egui::Context) {
        let paths = self.selected_or_cursor();
        let text = if paths.is_empty() { self.explorer.folder.to_string_lossy().into_owned() } else { paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n") };
        ctx.copy_text(text);
    }

    /// Where pasting goes: the folder the focused pane shows.
    pub fn here(&self) -> PathBuf {
        match self.focus {
            Pane::Files => self.explorer.files_folder().to_path_buf(),
            Pane::Folders => self.explorer.folder.clone(),
        }
    }

    /// Paste files: the system clipboard's when there is one (what Dolphin
    /// copied, or ours), else Fractale's own.
    pub fn paste_into(&mut self, dest: PathBuf) {
        if self.system_clipboard {
            self.paste_request = Some(dest);
        } else {
            self.explorer.paste(dest);
        }
    }

    /// File shortcuts (no text field focused). True if handled.
    pub fn ops_key(&mut self, ctx: &egui::Context, key: Key, m: egui::Modifiers) -> bool {
        let cmd = m.command;
        match key {
            Key::F2 => self.start_rename(),
            Key::Delete if m.shift => self.ask_delete(),
            Key::Delete => self.trash_selection(),
            Key::C if cmd && m.shift => self.copy_paths_as_text(ctx),
            Key::C if cmd => self.explorer.copy_selection(false),
            Key::X if cmd => self.explorer.copy_selection(true),
            Key::V if cmd => {
                let dest = self.here();
                self.paste_into(dest);
            }
            Key::N if cmd && m.shift => self.explorer.new_folder(),
            _ => return false,
        }
        true
    }

    /// Start renaming a new folder once it shows up.
    pub fn ops_frame(&mut self) {
        if let Some(name) = self.explorer.rename_request.clone() {
            if self.explorer.find(&name).is_some() {
                self.explorer.rename_request = None;
                if let Some(e) = self.explorer.find(&name) {
                    self.explorer.selection.only(Pane::of(e), &name);
                }
                self.start_rename();
            }
        }
        // The item went away (renamed elsewhere, deleted): stop editing.
        if self.ops.rename.as_ref().is_some_and(|r| self.explorer.is_current() && self.explorer.find_in(r.pane, &r.name).is_none()) {
            self.ops.rename = None;
        }
    }
}

/// The menu for the selection (right click on an item).
pub fn item_menu(ui: &mut egui::Ui, w: &mut Window) {
    let paths = w.selected_or_cursor();
    let single = paths.len() == 1;
    let folder = single && w.explorer.selection.cursor.as_ref().is_some_and(|(p, _)| *p == Pane::Folders);
    if ui.button("Open").clicked() {
        if let Some((p, n)) = w.explorer.selection.cursor.clone() {
            w.activate(p, &n);
        }
        ui.close();
    }
    if !folder && ui.button("Open with…").clicked() {
        w.ops.open_with = Some(OpenWith { files: paths.clone(), apps: None, mime: String::new(), command: String::new() });
        w.opener.send(crate::model::opener::Req::AppsFor(paths.clone()));
        ui.close();
    }
    ui.separator();
    if ui.add(egui::Button::new("Cut").shortcut_text(shortcut("X"))).clicked() {
        w.explorer.copy_selection(true);
        ui.close();
    }
    if ui.add(egui::Button::new("Copy").shortcut_text(shortcut("C"))).clicked() {
        w.explorer.copy_selection(false);
        ui.close();
    }
    if folder && (w.explorer.clipboard.is_some() || w.system_clipboard) && ui.button("Paste into this folder").clicked() {
        w.paste_into(paths[0].clone());
        ui.close();
    }
    if ui.add(egui::Button::new(if single { "Copy path" } else { "Copy paths" }).shortcut_text(shortcut("Shift+C"))).clicked() {
        w.copy_paths_as_text(ui.ctx());
        ui.close();
    }
    ui.separator();
    if single && ui.add(egui::Button::new("Rename…").shortcut_text("F2")).clicked() {
        w.start_rename();
        ui.close();
    }
    if ui.add(egui::Button::new("Move to trash").shortcut_text("Delete")).clicked() {
        w.trash_selection();
        ui.close();
    }
    if ui.add(egui::Button::new(RichText::new("Delete permanently…").color(CRITICAL)).shortcut_text("Shift+Delete")).clicked() {
        w.ask_delete();
        ui.close();
    }
}

/// The menu for empty space.
pub fn background_menu(ui: &mut egui::Ui, w: &mut Window) {
    let paste = egui::Button::new(match &w.explorer.clipboard {
        Some(c) => format!("Paste {} item{}", c.paths.len(), if c.paths.len() == 1 { "" } else { "s" }),
        None => "Paste".into(),
    })
    .shortcut_text(shortcut("V"));
    if ui.add_enabled(w.explorer.clipboard.is_some() || w.system_clipboard, paste).clicked() {
        let dest = w.explorer.files_folder().to_path_buf();
        w.paste_into(dest);
        ui.close();
    }
    if ui.add(egui::Button::new("New folder").shortcut_text(shortcut("Shift+N"))).clicked() {
        w.explorer.new_folder();
        ui.close();
    }
    ui.separator();
    if ui.add(egui::Button::new("Copy this folder's path").shortcut_text(shortcut("Shift+C"))).clicked() {
        ui.ctx().copy_text(w.explorer.files_folder().to_string_lossy().into_owned());
        ui.close();
    }
    let hidden = if w.explorer.show_hidden { "Hide hidden files" } else { "Show hidden files" };
    if ui.add(egui::Button::new(hidden).shortcut_text(shortcut("H"))).clicked() {
        w.explorer.toggle_hidden();
        ui.close();
    }
}

fn shortcut(k: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("Cmd+{k}")
    } else {
        format!("Ctrl+{k}")
    }
}

/// The name field over an item being renamed, in `rect`. True if this
/// item is being renamed (the caller skips drawing its name).
pub fn rename_field(ui: &mut egui::Ui, w: &mut Window, name: &OsStr, rect: egui::Rect) -> bool {
    let Some(r) = w.ops.rename.as_mut() else { return false };
    if r.name != name {
        return false;
    }
    let id = ui.scope_id().with(("rename", name));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let resp = child.add(egui::TextEdit::singleline(&mut r.text).id(id).desired_width(rect.width()).font(egui::TextStyle::Body).align(egui::Align2::CENTER_CENTER));
    if r.fresh {
        r.fresh = false;
        resp.request_focus();
        // Select the name without its extension (a folder's whole name).
        let is_dir = w.explorer.find_in(r.pane, name).is_some_and(|e| e.is_folder());
        let chars = r.text.chars().count();
        let stem = if is_dir { chars } else { r.text.rfind('.').filter(|&i| i > 0).map_or(chars, |i| r.text[..i].chars().count()) };
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(stem))));
            st.store(ui.ctx(), id);
        }
    }
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    if escape {
        w.ops.rename = None;
        return true;
    }
    if resp.lost_focus() {
        // Enter, Tab or a click elsewhere: keep the new name.
        let r = w.ops.rename.take().unwrap();
        let path = w.explorer.path_in(r.pane, &r.name);
        if !r.text.trim().is_empty() {
            w.explorer.rename(path, OsString::from(r.text.trim()));
        }
    }
    true
}

/// Make an item a drag source (dragging it carries the selection, or just
/// it if it isn't selected).
pub fn drag_source(ui: &egui::Ui, w: &mut Window, resp: &egui::Response, pane: Pane, name: &OsStr) {
    if resp.drag_started() {
        if !w.explorer.selection.is_selected(name) {
            w.explorer.selection.only(pane, name);
        }
        let paths = w.explorer.selected_paths();
        let devs = paths.iter().filter_map(|p| p.file_name()).filter_map(|n| w.explorer.find_in(pane, n)).map(|e| e.dev).collect();
        // Also a system drag, so other apps can take the files.
        w.system_drag = Some(paths.clone());
        resp.dnd_set_drag_payload(Dragged { paths, devs });
    }
    if resp.dragged() {
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            let n = w.explorer.selection.items.len().max(1);
            let label = if n == 1 { name.to_string_lossy().into_owned() } else { format!("{n} items") };
            let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("drag-label"));
            let painter = ui.ctx().layer_painter(layer);
            let g = painter.layout_no_wrap(label, font(12.5), TEXT);
            let r = egui::Rect::from_min_size(p + vec2(14.0, 10.0), g.size() + vec2(16.0, 8.0));
            painter.rect_filled(r, 6.0, RAISED);
            painter.galley(r.min + vec2(8.0, 4.0), g, TEXT);
        }
    }
}

/// Make a folder row a drop target; returns whether something is hovering
/// it (to highlight).
pub fn drop_target(ui: &egui::Ui, w: &mut Window, resp: &egui::Response, folder: &Path, dev: u64) -> bool {
    w.drop_rows.push((resp.rect, folder.to_path_buf()));
    let Some(d) = resp.dnd_hover_payload::<Dragged>() else { return false };
    // Not onto itself or into itself.
    if d.paths.iter().any(|p| folder.starts_with(p) || p.parent() == Some(folder)) {
        return false;
    }
    if let Some(d) = resp.dnd_release_payload::<Dragged>() {
        let m = ui.input(|i| i.modifiers);
        // Like other file managers: same disk moves, another disk copies;
        // Shift forces a move, Ctrl a copy.
        let kind = if m.shift {
            Kind::Move
        } else if m.command || d.devs.iter().any(|&s| s != dev) {
            Kind::Copy
        } else {
            Kind::Move
        };
        w.explorer.transfer(kind, d.paths.clone(), folder.to_path_buf());
    }
    true
}

/// The modal dialogs: delete confirmation, then a job's question.
pub fn dialogs(ctx: &egui::Context, w: &mut Window) {
    use crate::model::opener::Req;
    if let Some(path) = w.ops.ask_run.clone() {
        let mut close = None;
        egui::Modal::new(egui::Id::unique("ask-run")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new(format!("“{}” is a program. Run it?", path.file_name().unwrap_or_default().to_string_lossy())).strong().color(TEXT));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = Some(false);
                }
                if ui.button("Run").clicked() {
                    close = Some(true);
                }
            });
        });
        if let Some(run) = close {
            w.ops.ask_run = None;
            if run {
                w.opener.send(Req::Run(path));
            }
        }
        return;
    }
    if w.ops.open_with.is_some() {
        let mut close = false;
        let mut send = None;
        egui::Modal::new(egui::Id::unique("open-with")).show(ctx, |ui| {
            ui.set_width(400.0);
            let ow = w.ops.open_with.as_mut().unwrap();
            let what = if ow.files.len() == 1 { format!("“{}”", ow.files[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("{} files", ow.files.len()) };
            ui.label(RichText::new(format!("Open {what} with")).strong().color(TEXT));
            if !ow.mime.is_empty() {
                ui.label(RichText::new(&ow.mime).color(MUTED).size(11.5));
            }
            ui.add_space(6.0);
            match &ow.apps {
                None => {
                    ui.label(RichText::new("Looking…").color(MUTED));
                }
                Some(apps) if apps.is_empty() => {
                    ui.label(RichText::new("No installed app says it opens this.").color(MUTED));
                }
                Some(apps) => {
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for a in apps {
                            let label = if a.default { format!("{}  (default)", a.name) } else { a.name.clone() };
                            if ui.add(egui::Button::new(label).min_size(vec2(ui.available_width(), 24.0))).clicked() {
                                send = Some(Req::With { app: a.id.clone(), files: ow.files.clone() });
                            }
                        }
                    });
                }
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Other command").color(TEXT_2));
            let field = ui.add(egui::TextEdit::singleline(&mut ow.command).hint_text("e.g. gimp, or code --new-window").desired_width(f32::INFINITY));
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
                if (ui.add_enabled(!ow.command.trim().is_empty(), egui::Button::new("Run command")).clicked() || entered) && !ow.command.trim().is_empty() {
                    send = Some(Req::Custom { line: ow.command.clone(), files: ow.files.clone() });
                }
            });
        });
        if let Some(r) = send {
            w.opener.send(r);
            close = true;
        }
        if close {
            w.ops.open_with = None;
        }
        return;
    }
    if let Some(paths) = w.ops.confirm_delete.clone() {
        let counted = w.explorer.counted.as_ref().filter(|c| c.0 == paths).map(|c| (c.1, c.2));
        let mut close = None;
        egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
            ui.set_width(380.0);
            let what = if paths.len() == 1 { format!("“{}”", paths[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("{} items", paths.len()) };
            ui.label(RichText::new(format!("Delete {what} permanently?")).strong().color(TEXT));
            ui.add_space(4.0);
            let detail = match counted {
                Some((items, b)) => format!("{} file{} and folder{}, {}. This can't be undone.", super::widgets::thousands(items), if items == 1 { "" } else { "s" }, if items == 1 { "" } else { "s" }, bytes(b as f64)),
                None => "Counting…".into(),
            };
            ui.label(RichText::new(detail).color(TEXT_2));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = Some(false);
                }
                if ui.button(RichText::new("Delete").color(CRITICAL)).clicked() {
                    close = Some(true);
                }
            });
        });
        if let Some(go) = close {
            w.ops.confirm_delete = None;
            if go {
                w.explorer.delete(paths);
            }
        }
        return;
    }
    let Some(job) = w.explorer.jobs.views().into_iter().find(|j| j.question.is_some()) else { return };
    let q = job.question.clone().unwrap();
    egui::Modal::new(egui::Id::unique(("job-question", job.id))).show(ctx, |ui| {
        ui.set_width(440.0);
        match &q {
            Question::Conflict { source, target, source_brief, target_brief } => {
                let name = target.file_name().unwrap_or_default().to_string_lossy();
                let dir = target.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy();
                ui.label(RichText::new(format!("“{name}” already exists in {dir}")).strong().color(TEXT));
                ui.add_space(6.0);
                let side = |what: &str, b: &crate::model::ops::Brief| {
                    let kind = if b.is_dir { "folder".to_string() } else { bytes(b.size as f64) };
                    format!("{what}: {kind}, modified {}", super::widgets::date(b.mtime))
                };
                ui.label(RichText::new(side("Existing", target_brief)).color(TEXT_2));
                ui.label(RichText::new(side("Incoming", source_brief)).color(TEXT_2));
                ui.label(RichText::new(format!("from {}", source.parent().unwrap_or(Path::new("")).display())).color(MUTED));
                ui.add_space(8.0);
                ui.checkbox(&mut w.ops.apply_all, "Do this for every conflict in this job");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let all = w.ops.apply_all;
                    let mut pick = |r| {
                        w.explorer.jobs.resolve(job.id, r, all);
                        w.ops.apply_all = false;
                    };
                    if ui.button("Replace").clicked() {
                        pick(Resolution::Replace);
                    }
                    if ui.button("Skip").clicked() {
                        pick(Resolution::Skip);
                    }
                    if ui.button("Keep both").on_hover_text("Copy it as “name (2)”").clicked() {
                        pick(Resolution::KeepBoth);
                    }
                    ui.add_space(12.0);
                    if ui.button("Cancel").clicked() {
                        w.explorer.jobs.cancel(job.id);
                    }
                });
            }
            Question::Error { path, what, error } => {
                ui.label(RichText::new(format!("{what} failed")).strong().color(TEXT));
                ui.add_space(4.0);
                ui.label(RichText::new(path.display().to_string()).color(TEXT_2));
                ui.label(RichText::new(error).color(SERIOUS));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Retry").clicked() {
                        w.explorer.jobs.error_choice(job.id, ErrorChoice::Retry);
                    }
                    if ui.button("Skip").clicked() {
                        w.explorer.jobs.error_choice(job.id, ErrorChoice::Skip);
                    }
                    if ui.button("Cancel").clicked() {
                        w.explorer.jobs.error_choice(job.id, ErrorChoice::Cancel);
                    }
                });
            }
        }
    });
}

/// Running and unsuccessful jobs, above the status line.
pub fn jobs_strip(ui: &mut egui::Ui, w: &mut Window) {
    let jobs = w.explorer.jobs.views();
    if jobs.is_empty() {
        return;
    }
    for j in jobs {
        ui.horizontal(|ui| {
            ui.set_height(24.0);
            ui.label(RichText::new(describe(&j)).color(TEXT).size(12.5));
            let p = &j.progress;
            if j.outcome.is_none() {
                let frac = if p.bytes_total > 0 { p.bytes_done as f32 / p.bytes_total as f32 } else if p.items_total > 0 { p.items_done as f32 / p.items_total as f32 } else { 0.0 };
                let (rect, _) = ui.allocate_exact_size(vec2(160.0, 6.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, TRACK);
                if !p.counting {
                    ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, vec2(rect.width() * frac.clamp(0.0, 1.0), rect.height())), 3.0, SIZE);
                }
                let mut detail = if j.queued {
                    "waiting for the disk".to_string()
                } else if p.counting {
                    "counting…".to_string()
                } else if p.bytes_total > 0 {
                    format!("{} of {}", bytes(p.bytes_done as f64), bytes(p.bytes_total as f64))
                } else {
                    format!("{} of {}", p.items_done, p.items_total)
                };
                if let Some(left) = p.left() {
                    detail.push_str(&format!(" · {} left", time_left(left)));
                }
                ui.label(RichText::new(detail).color(MUTED).size(12.0));
                if ui.small_button("Cancel").clicked() {
                    w.explorer.jobs.cancel(j.id);
                }
            } else if ui.small_button("Dismiss").clicked() {
                w.explorer.jobs.dismiss(j.id);
            }
        });
    }
    ui.add_space(2.0);
}

fn describe(j: &JobView) -> String {
    let n = j.spec.sources.len();
    let what = if n == 1 { format!("“{}”", j.spec.sources[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("{n} items") };
    let to = j.spec.dest.as_ref().and_then(|d| d.file_name()).map(|d| format!(" to {}", d.to_string_lossy())).unwrap_or_default();
    match &j.outcome {
        None => format!("{} {what}{to}", j.spec.kind.verb()),
        Some(o) if o.cancelled => format!("{} {what}{to}: cancelled after {} done", j.spec.kind.verb(), o.done),
        Some(o) => format!("{} {what}{to}: {} done, {} skipped", j.spec.kind.verb(), o.done, o.skipped),
    }
}

fn time_left(d: std::time::Duration) -> String {
    match d.as_secs() {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s.div_ceil(60)),
        s => format!("{} h {} min", s / 3600, s / 60 % 60),
    }
}
