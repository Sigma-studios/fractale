//! The right pane: everything that isn't a folder, as a grid of tiles or a
//! list. Only the visible rows are laid out, so a 10k-file folder costs the
//! same per frame as a small one.

use super::icons;
use super::theme::*;
use super::widgets::{date, pane_header, scroll_target, sort_choices, Message};
use super::previews::Previews;
use super::Window;
use crate::model::entry::{Entry, Kind};
use crate::model::explorer::Layout as FileLayout;
use crate::model::view::{Pane, SortBy};
use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind};
use std::os::unix::ffi::OsStrExt;

pub const MIN_TILE: f32 = 64.0;
pub const MAX_TILE: f32 = 256.0;
const LIST_ROW_H: f32 = 26.0;

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    let n = w.explorer.files.len();
    pane_header(ui, &format!("Files ({n})"), |ui| {
        layout_toggle(ui, w);
        ui.add_space(8.0);
        let mut sort = w.explorer.file_sort;
        if sort_choices(ui, &mut sort, &[SortBy::Type, SortBy::Date, SortBy::Size, SortBy::Name]) {
            w.explorer.file_sort = sort;
            w.explorer.rebuild();
        }
    });
    if let Some(m) = Message::for_pane(&w.explorer, n, if w.explorer.folders.is_empty() { "This folder is empty" } else { "No files here" }) {
        m.show(ui);
        return;
    }
    // Ctrl+wheel (egui reports it as zoom) resizes tiles.
    if ui.rect_contains_pointer(ui.max_rect()) {
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            w.tile = (w.tile * zoom).clamp(MIN_TILE, MAX_TILE);
        }
    }
    match w.explorer.layout {
        FileLayout::Grid => grid(ui, w),
        FileLayout::List => list(ui, w),
    }
}

fn layout_toggle(ui: &mut egui::Ui, w: &mut Window) {
    for (layout, tip) in [(FileLayout::List, "List (names, sizes, dates)"), (FileLayout::Grid, "Grid of previews")] {
        let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
        let active = w.explorer.layout == layout;
        if active || resp.hovered() {
            ui.painter().rect_filled(rect, 5.0, if active { RAISED } else { SURFACE_HOVER });
        }
        let c = if active { TEXT } else { MUTED };
        match layout {
            FileLayout::Grid => icons::grid_glyph(ui.painter(), rect, c),
            FileLayout::List => icons::list_glyph(ui.painter(), rect, c),
        }
        if resp.on_hover_text(tip).clicked() {
            w.explorer.layout = layout;
        }
    }
}

fn cursor_row(w: &Window) -> Option<usize> {
    w.explorer
        .selection
        .cursor
        .as_ref()
        .filter(|(p, _)| *p == Pane::Files)
        .and_then(|(_, name)| w.explorer.files.iter().position(|&i| &w.explorer.entry(i).name == name))
}

fn tile_size(w: &Window) -> (f32, f32) {
    let icon = w.tile;
    let width = (icon + 28.0).max(108.0);
    let height = icon + 12.0 + 34.0 + 18.0;
    (width, height)
}

fn grid(ui: &mut egui::Ui, w: &mut Window) {
    let idx = Window::pane_index(Pane::Files);
    let (tw, th) = tile_size(w);
    let avail = ui.available_width() - 20.0;
    let columns = ((avail / tw).floor() as usize).max(1);
    let gap = ((avail - columns as f32 * tw) / columns as f32).max(0.0);
    let n = w.explorer.files.len();
    let rows = n.div_ceil(columns);
    w.scroll[idx].columns = columns;
    w.scroll[idx].row_height = th;
    let mut area = egui::ScrollArea::vertical().id_salt("files-scroll").auto_shrink([false, false]);
    let cursor = cursor_row(w);
    if let Some(y) = scroll_target(&mut w.scroll[idx], cursor, w.explorer.reveal_cursor) {
        area = area.vertical_scroll_offset(y);
    }
    background_menu(ui, w);
    let mut clicked_tile = false;
    // show_rows spaces rows by the spacing in effect now; rows touch.
    ui.spacing_mut().item_spacing.y = 0.0;
    let out = area.show_rows(ui, th, rows, |ui, range| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for r in range.clone() {
            let (row_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), th), Sense::hover());
            for c in 0..columns {
                let k = r * columns + c;
                if k >= n {
                    break;
                }
                let i = w.explorer.files[k];
                let x = row_rect.left() + 10.0 + c as f32 * (tw + gap) + gap / 2.0;
                let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(tw, th));
                clicked_tile |= tile(ui, w, i, rect);
            }
        }
        // A screen ahead (and a row behind), so scrolling finds previews
        // ready.
        let ahead = range.end..(range.end + range.len().max(1)).min(rows);
        let behind = range.start.saturating_sub(1)..range.start;
        for r in ahead.chain(behind) {
            for k in r * columns..((r + 1) * columns).min(n) {
                let e = w.explorer.entry(w.explorer.files[k]);
                if let Some(key) = preview_key(w, e, ui.ctx().pixels_per_point()) {
                    let size = e.size;
                    w.previews.want(key, size);
                }
            }
        }
    });
    background_click(ui, w, out.inner_rect, clicked_tile);
    let s = &mut w.scroll[idx];
    s.offset = out.state.clamped_offset().y;
    s.viewport = out.inner_rect.height();
}

/// Right click on empty space. Added before the items, so they're on top
/// of it.
fn background_menu(ui: &mut egui::Ui, w: &mut Window) {
    let bg = ui.interact(ui.available_rect_before_wrap(), ui.scope_id().with("files-background"), Sense::click());
    bg.context_menu(|ui| super::ops::background_menu(ui, w));
}

/// A click on empty space clears the selection.
fn background_click(ui: &egui::Ui, w: &mut Window, area: Rect, on_item: bool) {
    let clicked = ui.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| area.contains(p)));
    if clicked && !on_item {
        w.focus = Pane::Files;
        let mods = ui.input(|i| i.modifiers.command || i.modifiers.shift);
        if !mods {
            w.explorer.selection.clear();
        }
    }
}

/// One tile; true if it was clicked.
fn tile(ui: &mut egui::Ui, w: &mut Window, i: usize, rect: Rect) -> bool {
    let e = w.explorer.entry(i).clone();
    // By name: a rebuild between press and release (sizes arriving, a
    // refresh) must not lose the click.
    let id = ui.scope_id().with(("tile", &e.name));
    let resp = ui.interact(rect.shrink(3.0), id, Sense::click_and_drag());
    let sel = w.explorer.selection.is_selected(&e.name);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel, e.name_lossy()));
    let selected = w.explorer.selection.is_selected(&e.name);
    let cursor = w.explorer.selection.is_cursor(Pane::Files, &e.name) && w.focus == Pane::Files;
    let p = ui.painter().clone();
    let card = rect.shrink(3.0);
    if selected {
        p.rect_filled(card, CornerRadius::same(8), SIZE.gamma_multiply(0.22));
    } else if resp.hovered() {
        p.rect_filled(card, CornerRadius::same(8), SURFACE_HOVER);
    }
    if cursor {
        p.rect_stroke(card, CornerRadius::same(8), Stroke::new(1.0, SIZE.gamma_multiply(0.7)), StrokeKind::Inside);
    }
    let icon_rect = Rect::from_min_size(pos2(rect.center().x - w.tile / 2.0, rect.top() + 8.0), vec2(w.tile, w.tile));
    let ppp = ui.ctx().pixels_per_point();
    let shown = preview_key(w, &e, ppp).and_then(|k| w.previews.get(&k, e.size));
    match shown {
        Some(s) => {
            draw_preview(&p, icon_rect, s.texture.id(), s.size / ppp, &e);
            if let Some(next) = s.next {
                ui.ctx().request_repaint_after(next);
            }
        }
        None => draw_icon(&p, icon_rect, &e),
    }
    // Name on two lines, cut in the middle if needed.
    let color = if e.is_hidden() { MUTED } else { TEXT };
    let text_w = rect.width() - 14.0;
    let g = w.names.layout(ui, &e.name_lossy(), text_w, 2, font(12.5), color);
    let name_top = icon_rect.bottom() + 6.0;
    let name_h = g.size().y;
    let field = Rect::from_min_size(pos2(rect.left() + 4.0, name_top - 2.0), vec2(rect.width() - 8.0, 22.0));
    if !super::ops::rename_field(ui, w, &e.name, field) {
        // Centred galleys are laid out around x = 0.
        p.galley(pos2(rect.center().x, name_top), g, color);
        p.text(pos2(rect.center().x, name_top + name_h + 1.0), Align2::CENTER_TOP, size_label(&e), font(11.5), MUTED);
    }
    let resp = hover(resp, &e);
    interactions(ui, w, &resp, &e.name);
    resp.clicked() || resp.double_clicked() || resp.secondary_clicked()
}

/// Which preview a file's tile shows (files and links to files; not empty
/// ones).
fn preview_key(w: &Window, e: &Entry, ppp: f32) -> Option<crate::model::thumbs::Key> {
    let file = match e.kind {
        Kind::File => true,
        Kind::Symlink => e.link.as_ref().is_some_and(|l| l.resolves_to == Some(Kind::File)),
        _ => false,
    };
    (file && e.size > 0).then(|| Previews::key(&w.explorer.folder.join(&e.name), e.mtime, w.tile * ppp))
}

/// A preview fitted into the icon square, at most at its own size (small
/// images aren't blown up blurry; pixel art is pre-scaled and may grow),
/// sitting on the square's bottom edge like icons do.
fn draw_preview(p: &egui::Painter, rect: Rect, tex: egui::TextureId, natural: egui::Vec2, e: &Entry) {
    let pixel_art = e.extension().is_some_and(|x| x.eq_ignore_ascii_case("pxl"));
    let k = (rect.width() / natural.x).min(rect.height() / natural.y);
    let k = if pixel_art { k } else { k.min(1.0) };
    let size = natural * k;
    let r = Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, rect.bottom() - size.y), size);
    p.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
    if e.kind == Kind::Symlink {
        icons::link_badge(p, rect, false);
    }
}

fn draw_icon(p: &egui::Painter, rect: Rect, e: &Entry) {
    let broken = e.link.as_ref().is_some_and(|l| l.resolves_to.is_none());
    if e.opens_as_folder() {
        icons::folder(p, rect.shrink(rect.width() * 0.12), icons::tint(crate::model::entry::Category::Folder));
    } else {
        let ext = e.extension().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
        icons::file(p, rect, e.category, &ext, broken);
    }
    if e.kind == Kind::Symlink {
        icons::link_badge(p, rect, broken);
    }
}

fn size_label(e: &Entry) -> String {
    match e.kind {
        Kind::Symlink => match &e.link {
            Some(l) if l.resolves_to.is_none() => "broken link".into(),
            _ => "link".into(),
        },
        Kind::Other => "special".into(),
        _ => bytes(e.bytes as f64),
    }
}

fn hover(resp: egui::Response, e: &Entry) -> egui::Response {
    resp.on_hover_ui(|ui| {
        ui.label(egui::RichText::new(e.name_lossy()).color(TEXT));
        if let Some(l) = &e.link {
            let state = if l.resolves_to.is_none() { " (missing)" } else { "" };
            ui.label(egui::RichText::new(format!("→ {}{state}", l.target.display())).color(if state.is_empty() { TEXT_2 } else { CRITICAL }));
        } else {
            let mut line = format!("{} · {}", bytes(e.bytes as f64), e.category.label());
            if e.size != e.bytes && e.kind == Kind::File {
                // Sparse or compressed: what it claims vs what it uses.
                line = format!("{} on disk ({} apparent) · {}", bytes(e.bytes as f64), bytes(e.size as f64), e.category.label());
            }
            ui.label(egui::RichText::new(line).color(TEXT_2));
        }
        ui.label(egui::RichText::new(format!("Modified {}", date(e.mtime))).color(MUTED));
    })
}

fn interactions(ui: &egui::Ui, w: &mut Window, resp: &egui::Response, name: &std::ffi::OsStr) {
    super::ops::drag_source(ui, w, resp, Pane::Files, name);
    resp.context_menu(|ui| super::ops::item_menu(ui, w));
    let mods = super::widgets::click_mods(ui);
    if resp.clicked() {
        w.focus = Pane::Files;
        w.explorer.click(Pane::Files, name, mods);
    }
    if resp.double_clicked() {
        w.focus = Pane::Files;
        w.explorer.selection.only(Pane::Files, name);
        w.activate(Pane::Files, name);
    }
    if resp.secondary_clicked() {
        w.focus = Pane::Files;
        if !w.explorer.selection.is_selected(name) {
            w.explorer.selection.only(Pane::Files, name);
        }
    }
}

fn list(ui: &mut egui::Ui, w: &mut Window) {
    let idx = Window::pane_index(Pane::Files);
    let n = w.explorer.files.len();
    let width = ui.available_width();
    // Columns: name gets what's left.
    let (size_w, type_w, date_w) = (90.0, 110.0, 140.0);
    let name_w = (width - size_w - type_w - date_w - 60.0).max(120.0);
    // Header.
    ui.horizontal(|ui| {
        ui.add_space(14.0 + 26.0);
        let mut sort = w.explorer.file_sort;
        let mut changed = false;
        for (by, wid, label) in [(SortBy::Name, name_w, "Name"), (SortBy::Size, size_w, "Size"), (SortBy::Type, type_w, "Type"), (SortBy::Date, date_w, "Modified")] {
            let active = sort.by == Some(by);
            let (rect, resp) = ui.allocate_exact_size(vec2(wid, 20.0), Sense::click());
            let align = if by == SortBy::Size { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            let x = if by == SortBy::Size { rect.right() - 22.0 } else { rect.left() };
            let color = if active { TEXT } else { MUTED };
            let r = ui.painter().text(pos2(x, rect.center().y), align, label, font(12.0), color);
            if active {
                super::widgets::sort_arrow(ui.painter(), pos2(r.right() + 8.0, rect.center().y), sort.reversed, color);
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                sort.click(by);
                changed = true;
            }
        }
        if changed {
            w.explorer.file_sort = sort;
            w.explorer.rebuild();
        }
    });
    w.scroll[idx].columns = 1;
    w.scroll[idx].row_height = LIST_ROW_H;
    let mut area = egui::ScrollArea::vertical().id_salt("files-list").auto_shrink([false, false]);
    let cursor = cursor_row(w);
    if let Some(y) = scroll_target(&mut w.scroll[idx], cursor, w.explorer.reveal_cursor) {
        area = area.vertical_scroll_offset(y);
    }
    background_menu(ui, w);
    let mut clicked_row = false;
    // show_rows spaces rows by the spacing in effect now; rows touch.
    ui.spacing_mut().item_spacing.y = 0.0;
    let out = area.show_rows(ui, LIST_ROW_H, n, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for k in range {
            let i = w.explorer.files[k];
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), LIST_ROW_H), Sense::click_and_drag());
            let e = w.explorer.entry(i).clone();
            let sel = w.explorer.selection.is_selected(&e.name);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel, e.name_lossy()));
            let selected = w.explorer.selection.is_selected(&e.name);
            let cursor = w.explorer.selection.is_cursor(Pane::Files, &e.name) && w.focus == Pane::Files;
            let p = ui.painter().clone();
            let inner = rect.shrink2(vec2(6.0, 1.0));
            if selected {
                p.rect_filled(inner, CornerRadius::same(5), SIZE.gamma_multiply(0.22));
            } else if resp.hovered() {
                p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
            }
            if cursor {
                p.rect_stroke(inner, CornerRadius::same(5), Stroke::new(1.0, SIZE.gamma_multiply(0.7)), StrokeKind::Inside);
            }
            let icon = Rect::from_center_size(pos2(inner.left() + 20.0, inner.center().y), vec2(20.0, 20.0));
            draw_icon(&p, icon, &e);
            let mut x = inner.left() + 40.0;
            let color = if e.is_hidden() { MUTED } else { TEXT };
            let field = Rect::from_min_size(pos2(x - 4.0, inner.top()), vec2(name_w - 8.0, inner.height()));
            if !super::ops::rename_field(ui, w, &e.name, field) {
                let g = super::text::one_line(ui, &e.name_lossy(), name_w - 12.0, font(13.0), color);
                p.galley(pos2(x, inner.center().y - g.size().y / 2.0), g, color);
            }
            x += name_w;
            p.text(pos2(x + size_w - 10.0, inner.center().y), Align2::RIGHT_CENTER, size_label(&e), font(12.0), TEXT_2);
            x += size_w;
            let ty = e.extension().map(|x| String::from_utf8_lossy(x.as_bytes()).to_uppercase()).map(|x| format!("{} {x}", e.category.label())).unwrap_or_else(|| e.category.label().to_string());
            let tg = super::text::one_line(ui, &ty, type_w - 10.0, font(12.0), MUTED);
            p.galley(pos2(x, inner.center().y - tg.size().y / 2.0), tg, MUTED);
            x += type_w;
            p.text(pos2(x, inner.center().y), Align2::LEFT_CENTER, date(e.mtime), font(12.0), MUTED);
            let resp = hover(resp, &e);
            interactions(ui, w, &resp, &e.name);
            clicked_row |= resp.clicked() || resp.double_clicked() || resp.secondary_clicked();
        }
    });
    background_click(ui, w, out.inner_rect, clicked_row);
    let s = &mut w.scroll[idx];
    s.offset = out.state.clamped_offset().y;
    s.viewport = out.inner_rect.height();
}
