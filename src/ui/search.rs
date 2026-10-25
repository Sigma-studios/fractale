//! Search results, shown instead of both panes while there's a query: one
//! list with the name, where it is (relative to the open folder), its kind
//! and size. Enter opens a result; Alt+Enter opens its folder with it
//! selected.

use super::icons;
use super::theme::*;
use super::widgets::{pane_header, thousands};
use super::Window;
use crate::model::entry::{Category, Kind};
use crate::model::explorer::Activate;
use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, Rect, RichText, Sense, Stroke, StrokeKind};
use std::path::Path;

const ROW_H: f32 = 34.0;

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    let Some(view) = w.explorer.search.clone() else { return };
    let count = view.hits.len();
    let state = match (view.done, view.partial, view.indexed) {
        (false, _, _) => "searching…".to_string(),
        (true, true, _) => "partial: the search stopped early, there may be more".to_string(),
        (true, false, true) => "from répertoire's index".to_string(),
        (true, false, false) => "searched this folder".to_string(),
    };
    let scope = if view.everywhere { "everywhere".to_string() } else { "here".to_string() };
    pane_header(ui, &format!("Results ({}) {scope}", thousands(count as u64)), |ui| {
        ui.label(RichText::new(state).font(font(12.0)).color(MUTED));
    });
    if count == 0 {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| ui.label(RichText::new(if view.done { "Nothing found" } else { "Searching…" }).color(FAINT)));
        return;
    }
    let base = w.explorer.folder.clone();
    let mut open: Option<(usize, bool)> = None;
    let cursor_y = view.cursor as f32 * ROW_H;
    let mut area = egui::ScrollArea::vertical().id_salt("search-results").auto_shrink([false, false]);
    if w.search_reveal {
        let s = &w.scroll[1];
        if cursor_y < s.offset {
            area = area.vertical_scroll_offset(cursor_y);
        } else if cursor_y + ROW_H > s.offset + s.viewport {
            area = area.vertical_scroll_offset(cursor_y + ROW_H - s.viewport);
        }
        w.search_reveal = false;
    }
    // show_rows spaces rows by the spacing in effect now; rows touch.
    ui.spacing_mut().item_spacing.y = 0.0;
    let out = area.show_rows(ui, ROW_H, count, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let hit = &view.hits[i];
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let name = hit.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, i == view.cursor, &name));
            let p = ui.painter().clone();
            let inner = rect.shrink2(vec2(8.0, 1.0));
            if i == view.cursor {
                p.rect_filled(inner, CornerRadius::same(5), SIZE.gamma_multiply(0.22));
                p.rect_stroke(inner, CornerRadius::same(5), Stroke::new(1.0, SIZE.gamma_multiply(0.6)), StrokeKind::Inside);
            } else if resp.hovered() {
                p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
            }
            let icon = Rect::from_center_size(pos2(inner.left() + 18.0, inner.center().y), vec2(20.0, 20.0));
            let ext = hit.path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            if hit.kind == Kind::Dir {
                icons::folder(&p, icon.shrink(2.0), icons::tint(Category::Folder));
            } else {
                let cat = Category::of(hit.path.file_name().unwrap_or_default(), hit.kind, 0o644, None);
                icons::file(&p, icon, cat, &ext, false);
            }
            let size = match (hit.kind, hit.bytes) {
                (_, Some(b)) => bytes(b as f64),
                (Kind::Dir, None) => "folder".into(),
                _ => String::new(),
            };
            let size_r = p.text(pos2(inner.right() - 10.0, inner.center().y), Align2::RIGHT_CENTER, size, font(12.0), TEXT_2);
            let x = icon.right() + 10.0;
            let avail = size_r.left() - 20.0 - x;
            let g = super::text::one_line(ui, &name, avail * 0.45, font(13.0), TEXT);
            let name_w = g.size().x;
            p.galley(pos2(x, inner.center().y - g.size().y / 2.0), g, TEXT);
            let where_ = relative(&hit.path, &base);
            let g2 = super::text::one_line(ui, &where_, avail - name_w - 16.0, font(12.0), MUTED);
            p.galley(pos2(x + name_w + 16.0, inner.center().y - g2.size().y / 2.0), g2, MUTED);
            let resp = resp.on_hover_text(hit.path.display().to_string());
            if resp.clicked() {
                if let Some(v) = w.explorer.search.as_mut() {
                    v.cursor = i;
                }
            }
            if resp.double_clicked() {
                open = Some((i, false));
            }
        }
    });
    w.scroll[1].offset = out.state.clamped_offset().y;
    w.scroll[1].viewport = out.inner_rect.height();
    if let Some((i, in_folder)) = open {
        open_result(w, i, in_folder);
    }
}

/// Where a hit is, relative to the open folder ("in Projects/socks").
fn relative(path: &Path, base: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    match parent.strip_prefix(base) {
        Ok(r) if r.as_os_str().is_empty() => "here".into(),
        Ok(r) => format!("in {}", r.display()),
        Err(_) => format!("in {}", parent.display()),
    }
}

pub fn open_result(w: &mut Window, i: usize, in_folder: bool) {
    let Some(hit) = w.explorer.search.as_ref().and_then(|v| v.hits.get(i)).cloned() else { return };
    w.topbar.search.clear();
    if let Activate::File(path) = w.explorer.open_hit(&hit, in_folder) {
        w.opened.push(path);
    }
}

/// Up/Down/Enter while results are shown (even with the search field
/// focused: a one-line field has no use for them).
pub fn keys(ui: &egui::Ui, w: &mut Window) {
    let Some(len) = w.explorer.search.as_ref().map(|v| v.hits.len()) else { return };
    let (down, up, enter, alt, pgdn, pgup) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.key_pressed(egui::Key::Enter),
            i.modifiers.alt,
            i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
        )
    });
    let page = ((w.scroll[1].viewport / ROW_H) as usize).max(1);
    if let Some(v) = w.explorer.search.as_mut() {
        let before = v.cursor;
        if down {
            v.cursor = (v.cursor + 1).min(len.saturating_sub(1));
        }
        if up {
            v.cursor = v.cursor.saturating_sub(1);
        }
        if pgdn {
            v.cursor = (v.cursor + page).min(len.saturating_sub(1));
        }
        if pgup {
            v.cursor = v.cursor.saturating_sub(page);
        }
        if v.cursor != before {
            w.search_reveal = true;
        }
    }
    if enter && len > 0 {
        let i = w.explorer.search.as_ref().map_or(0, |v| v.cursor);
        open_result(w, i, alt);
    }
}
