//! The left pane: subfolders as rows with size bars. A click opens the
//! folder; the chevron expands it in place (its subfolders as indented rows,
//! bars as a share of their parent row), the quick way to follow the
//! longest bar down. Ctrl and Shift clicks select.
//!
//! Each bar is the folder's share of the open folder's total, so the bars
//! and the extra rows under them (files here, unreadable, mounted here)
//! account for the whole folder.

use super::icons;
use super::theme::*;
use super::widgets::{pane_header, scroll_target, sort_choices, thousands, Message};
use super::Window;
use crate::model::entry::Category;
use crate::model::listing::Count;
use crate::model::sizes::ChildSize;
use crate::model::view::{Mods, Pane, SortBy};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const ROW_H: f32 = 30.0;
const INDENT: f32 = 18.0;

#[derive(Clone, Debug)]
pub enum RowKind {
    /// A subfolder: top level (selectable) or nested in an expanded row.
    Folder { path: PathBuf, name: OsString, top: bool, size: Option<ChildSize>, count: Option<Count>, mtime: i64, hidden: bool },
    /// The files directly in a folder (and its own entry).
    FilesHere(u64),
    /// Space the size source can't attribute, or subfolders it couldn't
    /// read.
    Unreadable { bytes: u64, folders: usize },
    /// A subfolder that is another filesystem.
    Mount { path: PathBuf, name: OsString, size: Option<ChildSize> },
}

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub depth: usize,
    /// The total the bar is a share of.
    pub of: Option<u64>,
}

impl Row {
    /// Bytes this row stands for (what its bar shows).
    pub fn bytes(&self) -> Option<u64> {
        match &self.kind {
            RowKind::Folder { size, .. } => size.as_ref().map(|s| s.total),
            RowKind::FilesHere(b) => Some(*b),
            RowKind::Unreadable { bytes, .. } => Some(*bytes),
            RowKind::Mount { .. } => None,
        }
    }
}

/// The rows to show, top to bottom.
pub fn rows(w: &Window) -> Vec<Row> {
    let ex = &w.explorer;
    let open = ex.folder_sizes();
    let of = open.map(|s| s.total).filter(|&t| t > 0);
    let mut out = Vec::new();
    let mut mounts = Vec::new();
    for &i in &ex.folders {
        let e = ex.entry(i);
        let path = ex.folder.join(&e.name);
        let size = open.and_then(|s| s.children.get(&e.name)).cloned();
        if ex.listing.dev != 0 && e.dev != ex.listing.dev {
            mounts.push(Row { kind: RowKind::Mount { path, name: e.name.clone(), size }, depth: 0, of: None });
            continue;
        }
        let expanded = ex.is_expanded(&path);
        let total = size.as_ref().map(|s| s.total);
        out.push(Row {
            kind: RowKind::Folder { path: path.clone(), name: e.name.clone(), top: true, size, count: ex.counts.get(&e.name).copied(), mtime: e.mtime, hidden: e.is_hidden() },
            depth: 0,
            of,
        });
        if expanded {
            nested(w, &path, total, 1, &mut out);
        }
    }
    if let Some(s) = open {
        if s.files_here > 0 {
            out.push(Row { kind: RowKind::FilesHere(s.files_here), depth: 0, of });
        }
        let unreadable = s.children.values().filter(|c| c.unreadable).count();
        if s.hidden > 0 || unreadable > 0 {
            out.push(Row { kind: RowKind::Unreadable { bytes: s.hidden, folders: unreadable }, depth: 0, of });
        }
    }
    out.extend(mounts);
    out
}

/// An expanded folder's subfolders, largest first, then its files.
fn nested(w: &Window, path: &Path, total: Option<u64>, depth: usize, out: &mut Vec<Row>) {
    let Some(s) = w.explorer.sizes.get(path) else { return };
    let hidden = |n: &OsString| n.to_string_lossy().starts_with('.');
    let mut kids: Vec<(&OsString, &ChildSize)> = s.children.iter().filter(|(n, c)| c.mount.is_none() && (w.explorer.show_hidden || !hidden(n))).collect();
    kids.sort_by(|a, b| b.1.total.cmp(&a.1.total).then_with(|| a.0.cmp(b.0)));
    let of = total.or(Some(s.total)).filter(|&t| t > 0);
    for (name, c) in kids {
        let p = path.join(name);
        let expanded = w.explorer.is_expanded(&p);
        out.push(Row {
            kind: RowKind::Folder { path: p.clone(), name: name.clone(), top: false, size: Some(c.clone()), count: None, mtime: c.mtime, hidden: hidden(name) },
            depth,
            of,
        });
        if expanded {
            nested(w, &p, Some(c.total), depth + 1, out);
        }
    }
    if s.files_here > 0 {
        out.push(Row { kind: RowKind::FilesHere(s.files_here), depth, of });
    }
}

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    let n = w.explorer.folders.len();
    let total = w.explorer.folder_sizes().map(|s| (s.total, s.complete));
    // "Biggest folders here" needs répertoire (otherwise a full walk).
    if w.show_top && (!w.explorer.uses_daemon() || w.top_for.as_ref() != Some(&w.explorer.folder)) {
        w.show_top = false;
    }
    let title = if w.show_top { "Biggest folders here".to_string() } else { format!("Folders ({})", thousands(n as u64)) };
    pane_header(ui, &title, |ui| {
        if w.explorer.uses_daemon() {
            let label = if w.show_top { "back to folders" } else { "biggest" };
            let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), font(12.0), TEXT_2));
            let (r, resp) = ui.allocate_exact_size(g.size() + vec2(12.0, 8.0), Sense::click());
            ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, if resp.hovered() { MUTED } else { BORDER }), StrokeKind::Inside);
            ui.painter().galley(r.center() - g.size() / 2.0, g, TEXT_2);
            let resp = resp.on_hover_text("The 20 largest folders anywhere below this one (répertoire)").on_hover_cursor(egui::CursorIcon::PointingHand);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            if resp.clicked() {
                w.show_top = !w.show_top;
                if w.show_top {
                    w.top_for = Some(w.explorer.folder.clone());
                    w.explorer.request_top();
                }
            }
            ui.add_space(8.0);
        }
        if w.show_top {
            return;
        }
        if let Some((t, complete)) = total {
            let text = if complete { bytes(t as f64) } else { format!("≥ {}", bytes(t as f64)) };
            ui.label(egui::RichText::new(text).font(strong(13.0)).color(TEXT)).on_hover_text(if complete { "This folder's total" } else { "Still measuring: at least this much" });
            ui.add_space(10.0);
        }
        let mut sort = w.explorer.folder_sort;
        if sort_choices(ui, &mut sort, &[SortBy::Date, SortBy::Name, SortBy::Size]) {
            w.explorer.folder_sort = sort;
            w.explorer.rebuild();
        }
    });
    if w.show_top {
        top_list(ui, w, total.map(|t| t.0));
        return;
    }
    let rows = rows(w);
    if rows.is_empty() {
        if let Some(m) = Message::for_pane(&w.explorer, n, "No subfolders") {
            m.show(ui);
            return;
        }
    }
    // Don't reorder rows while the pointer is over them.
    w.explorer.freeze_folders = ui.rect_contains_pointer(ui.max_rect());
    let idx = Window::pane_index(Pane::Folders);
    let cursor_row = w.explorer.selection.cursor.as_ref().filter(|(p, _)| *p == Pane::Folders).and_then(|(_, name)| {
        rows.iter().position(|r| matches!(&r.kind, RowKind::Folder { name: n, top: true, .. } if n == name))
    });
    let mut area = egui::ScrollArea::vertical().id_salt("folders-scroll").auto_shrink([false, false]);
    if let Some(y) = scroll_target(&mut w.scroll[idx], cursor_row, w.explorer.reveal_cursor) {
        area = area.vertical_scroll_offset(y);
    }
    let mut visible = Vec::new();
    let out = area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for r in range {
            if let RowKind::Folder { name, top: true, .. } = &rows[r].kind {
                visible.push(name.clone());
            }
            row(ui, w, &rows[r]);
        }
    });
    // Rows on screen get sized first.
    if visible != w.visible_folders {
        w.visible_folders = visible.clone();
        w.explorer.prioritize(visible);
    }
    let s = &mut w.scroll[idx];
    s.offset = out.state.clamped_offset().y;
    s.viewport = out.inner_rect.height();
    s.row_height = ROW_H;
    s.columns = 1;
}

fn row(ui: &mut egui::Ui, w: &mut Window, r: &Row) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    let inner = rect.shrink2(vec2(6.0, 1.0));
    let indent = r.depth as f32 * INDENT;
    let p = ui.painter().clone();
    match &r.kind {
        RowKind::Folder { path, name, top, size, count, mtime, hidden } => {
            let label = name.to_string_lossy().into_owned();
            let selected = *top && w.explorer.selection.is_selected(name);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
            let cursor = *top && w.explorer.selection.is_cursor(Pane::Folders, name) && w.focus == Pane::Folders;
            if selected {
                p.rect_filled(inner, CornerRadius::same(5), SIZE.gamma_multiply(0.22));
            } else if resp.hovered() {
                p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
            }
            if cursor {
                p.rect_stroke(inner, CornerRadius::same(5), Stroke::new(1.0, SIZE.gamma_multiply(0.7)), StrokeKind::Inside);
            }
            // The chevron expands in place; a folder known to have no
            // subfolders (or that can't be read) has none.
            let expandable = size.as_ref().is_none_or(|s| s.dirs > 0 && !s.unreadable);
            let chev = Rect::from_min_size(pos2(inner.left() + 2.0 + indent, inner.top()), vec2(20.0, inner.height()));
            let chev_resp = ui.interact(chev, ui.scope_id().with(("chevron", path)), Sense::click());
            if expandable {
                let open = w.explorer.is_expanded(path);
                icons::chevron(&p, chev, open, if chev_resp.hovered() { TEXT } else { MUTED });
                chev_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("expand {label}")));
            }
            let glyph = Rect::from_center_size(pos2(chev.right() + 10.0, inner.center().y), vec2(16.0, 14.0));
            let unreadable = size.as_ref().is_some_and(|s| s.unreadable) || matches!(count, Some(Count::Unreadable));
            let tint = if unreadable { FAINT } else { icons::tint(Category::Folder) };
            icons::folder(&p, glyph, tint.gamma_multiply(if *hidden { 0.5 } else { 1.0 }));
            let detail = match size {
                _ if unreadable => "unreadable".to_string(),
                Some(s) => bytes(s.total as f64),
                None => String::new(),
            };
            let name_right = bar(&p, inner, size.as_ref().map(|s| s.total), r.of, &detail, unreadable);
            let name_x = glyph.right() + 8.0;
            let color = if *hidden { MUTED } else { TEXT };
            let g = super::text::one_line(ui, &label, (name_right - 10.0 - name_x).max(20.0), font(13.0), color);
            p.galley(pos2(name_x, inner.center().y - g.size().y / 2.0), g, color);

            let resp = resp.on_hover_ui(|ui| folder_tip(ui, &label, size.as_ref(), *count, r.of, *mtime));
            if expandable && chev_resp.clicked() {
                w.explorer.toggle_expanded(path.clone());
                return;
            }
            let mods = ui.input(|i| Mods { toggle: i.modifiers.command, range: i.modifiers.shift });
            if resp.clicked() {
                w.focus = Pane::Folders;
                if *top && (mods.toggle || mods.range) {
                    w.explorer.click(Pane::Folders, name, mods);
                } else if *top {
                    w.explorer.selection.only(Pane::Folders, name);
                    w.activate(Pane::Folders, name);
                } else {
                    w.explorer.open(path.clone());
                }
            }
            if resp.secondary_clicked() && *top {
                w.focus = Pane::Folders;
                if !selected {
                    w.explorer.selection.only(Pane::Folders, name);
                }
            }
        }
        RowKind::FilesHere(b) => {
            let x = inner.left() + 2.0 + indent + 20.0;
            let right = bar(&p, inner, Some(*b), r.of, &bytes(*b as f64), true);
            let g = super::text::one_line(ui, "files here", right - x - 28.0, font(13.0), MUTED);
            p.galley(pos2(x + 18.0, inner.center().y - g.size().y / 2.0), g, MUTED);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, "files here"));
            resp.on_hover_text("Files directly in this folder (and the folder's own entry)");
        }
        RowKind::Unreadable { bytes: b, folders } => {
            let x = inner.left() + 22.0;
            let (label, detail, tip) = if *b > 0 {
                ("unreadable".to_string(), bytes(*b as f64), "Space in folders you can't read (other users' files, system folders)")
            } else {
                (
                    format!("unreadable ({folders} folder{})", if *folders == 1 { "" } else { "s" }),
                    "?".to_string(),
                    "Folders that can't be read, so their contents aren't counted. With répertoire running as root, their size shows here.",
                )
            };
            let right = bar(&p, inner, (*b > 0).then_some(*b), r.of, &detail, true);
            let g = super::text::one_line(ui, &label, right - x - 28.0, font(13.0), MUTED);
            p.galley(pos2(x + 18.0, inner.center().y - g.size().y / 2.0), g, MUTED);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, "unreadable"));
            resp.on_hover_text(tip);
        }
        RowKind::Mount { path, name, size } => {
            let label = name.to_string_lossy().into_owned();
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
            if resp.hovered() {
                p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
            }
            let glyph = Rect::from_center_size(pos2(inner.left() + 32.0, inner.center().y), vec2(16.0, 14.0));
            icons::folder(&p, glyph, MUTED);
            let detail = size.as_ref().map(|s| bytes(s.total as f64)).unwrap_or_default();
            let t = p.text(pos2(inner.right() - 8.0, inner.center().y), Align2::RIGHT_CENTER, &detail, font(12.0), TEXT_2);
            let badge = p.text(pos2(t.left() - 14.0, inner.center().y), Align2::RIGHT_CENTER, "mounted here", font(11.0), MUTED);
            p.rect_stroke(badge.expand2(vec2(6.0, 2.0)), CornerRadius::same(4), Stroke::new(1.0, BORDER), StrokeKind::Outside);
            let x = glyph.right() + 8.0;
            let g = super::text::one_line(ui, &label, badge.left() - 20.0 - x, font(13.0), TEXT_2);
            p.galley(pos2(x, inner.center().y - g.size().y / 2.0), g, TEXT_2);
            let resp = resp.on_hover_text("Another filesystem is mounted here. Its size doesn't count in this folder's total.");
            if resp.clicked() {
                w.explorer.open(path.clone());
            }
        }
    }
}

/// The size bar and the number at the right of a row; returns the x where
/// the name must end. No bar while either number is unknown: a faint
/// placeholder instead, so rows don't change shape when sizes arrive.
fn bar(p: &egui::Painter, inner: Rect, part: Option<u64>, of: Option<u64>, detail: &str, muted: bool) -> f32 {
    let text_w = 74.0;
    let bar_w = (inner.width() * 0.3).clamp(60.0, 240.0);
    let text_right = inner.right() - 8.0;
    p.text(pos2(text_right, inner.center().y), Align2::RIGHT_CENTER, detail, font(12.0), if muted { MUTED } else { TEXT_2 });
    let bar_rect = Rect::from_min_size(pos2(text_right - text_w - bar_w, inner.center().y - 4.0), vec2(bar_w, 8.0));
    p.rect_filled(bar_rect, CornerRadius::same(3), TRACK);
    match (part, of) {
        (Some(part), Some(of)) => {
            let frac = (part as f64 / of as f64).clamp(0.0, 1.0) as f32;
            if frac > 0.0 {
                let fill = Rect::from_min_size(bar_rect.min, vec2((bar_w * frac).max(2.0), bar_rect.height()));
                let color: Color32 = if muted { OTHER } else { SIZE };
                p.rect_filled(fill, CornerRadius::same(3), color);
            }
        }
        _ => {
            let mut x = bar_rect.left() + 2.0;
            while x < bar_rect.right() - 6.0 {
                p.rect_filled(Rect::from_min_size(pos2(x, bar_rect.center().y - 1.0), vec2(6.0, 2.0)), 1.0, BORDER);
                x += 12.0;
            }
        }
    }
    bar_rect.left()
}

fn folder_tip(ui: &mut egui::Ui, name: &str, size: Option<&ChildSize>, count: Option<Count>, of: Option<u64>, mtime: i64) {
    ui.label(egui::RichText::new(name).color(TEXT));
    match size {
        Some(s) if s.unreadable => {
            ui.label(egui::RichText::new("Can't be read: its contents aren't counted").color(TEXT_2));
        }
        Some(s) => {
            let pct = of.map(|o| format!(" · {:.1}% of this folder", s.total as f64 / o.max(1) as f64 * 100.0)).unwrap_or_default();
            ui.label(egui::RichText::new(format!("{}{pct}", bytes(s.total as f64))).color(TEXT_2));
            ui.label(egui::RichText::new(format!("{} files in {} folders", thousands(s.files), thousands(s.dirs))).color(MUTED));
        }
        None => {
            if let Some(Count::Items(n)) = count {
                ui.label(egui::RichText::new(format!("{} items · measuring…", thousands(n))).color(TEXT_2));
            }
        }
    }
    ui.label(egui::RichText::new(format!("Modified {}", super::widgets::date(mtime))).color(MUTED));
}

/// répertoire's biggest folders anywhere below the open one, as a flat
/// list with paths relative to it and bars as a share of its total.
fn top_list(ui: &mut egui::Ui, w: &mut Window, total: Option<u64>) {
    let Some((_, folders)) = w.explorer.top.clone() else {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| ui.label(egui::RichText::new("Asking répertoire…").color(MUTED)));
        return;
    };
    let base = w.explorer.folder.clone();
    egui::ScrollArea::vertical().id_salt("top-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (path, bytes_) in folders {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let rel = path.strip_prefix(&base).map(|r| r.display().to_string()).unwrap_or_else(|_| path.display().to_string());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &rel));
            let inner = rect.shrink2(vec2(6.0, 1.0));
            let p = ui.painter().clone();
            if resp.hovered() {
                p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
            }
            let glyph = Rect::from_center_size(pos2(inner.left() + 18.0, inner.center().y), vec2(16.0, 14.0));
            icons::folder(&p, glyph, icons::tint(Category::Folder));
            let right = bar(&p, inner, Some(bytes_), total, &bytes(bytes_ as f64), false);
            let x = glyph.right() + 8.0;
            // The end of the path is what matters.
            let g = super::text::one_line(ui, &rel, right - 10.0 - x, font(13.0), TEXT);
            p.galley(pos2(x, inner.center().y - g.size().y / 2.0), g, TEXT);
            if resp.on_hover_text(path.display().to_string()).clicked() {
                w.show_top = false;
                w.explorer.open(path.clone());
            }
        }
    });
}
