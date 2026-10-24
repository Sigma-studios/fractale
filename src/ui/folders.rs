//! The left pane: subfolders as rows. A click opens the folder; Ctrl and
//! Shift clicks select. Sizes and bars come with the size source; until
//! then each row shows its item count.

use super::icons;
use super::theme::*;
use super::widgets::{pane_header, scroll_target, sort_choices, Message};
use super::Window;
use crate::model::listing::Count;
use crate::model::view::{Mods, Pane, SortBy};
use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind};

pub const ROW_H: f32 = 30.0;

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    let n = w.explorer.folders.len();
    pane_header(ui, &format!("Folders ({n})"), |ui| {
        let mut sort = w.explorer.folder_sort;
        if sort_choices(ui, &mut sort, &[SortBy::Date, SortBy::Name, SortBy::Size]) {
            w.explorer.folder_sort = sort;
            w.explorer.rebuild();
        }
    });
    if let Some(m) = Message::for_pane(&w.explorer, n, "No subfolders") {
        m.show(ui);
        return;
    }
    let idx = Window::pane_index(Pane::Folders);
    let cursor_row = w.explorer.selection.cursor.as_ref().filter(|(p, _)| *p == Pane::Folders).and_then(|(_, name)| {
        w.explorer.folders.iter().position(|&i| &w.explorer.entry(i).name == name)
    });
    let mut area = egui::ScrollArea::vertical().id_salt("folders-scroll").auto_shrink([false, false]);
    if let Some(y) = scroll_target(&mut w.scroll[idx], cursor_row, w.explorer.reveal_cursor) {
        area = area.vertical_scroll_offset(y);
    }
    let out = area.show_rows(ui, ROW_H, n, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in range {
            let i = w.explorer.folders[row];
            folder_row(ui, w, i);
        }
    });
    let s = &mut w.scroll[idx];
    s.offset = out.state.clamped_offset().y;
    s.viewport = out.inner_rect.height();
    s.row_height = ROW_H;
    s.columns = 1;
}

fn folder_row(ui: &mut egui::Ui, w: &mut Window, i: usize) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    let e = w.explorer.entry(i).clone();
    let selected_now = w.explorer.selection.is_selected(&e.name);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected_now, e.name_lossy()));
    let selected = w.explorer.selection.is_selected(&e.name);
    let cursor = w.explorer.selection.is_cursor(Pane::Folders, &e.name) && w.focus == Pane::Folders;
    let p = ui.painter();
    let inner = rect.shrink2(vec2(6.0, 1.0));
    if selected {
        p.rect_filled(inner, CornerRadius::same(5), SIZE.gamma_multiply(0.22));
    } else if resp.hovered() {
        p.rect_filled(inner, CornerRadius::same(5), SURFACE_HOVER);
    }
    if cursor {
        p.rect_stroke(inner, CornerRadius::same(5), Stroke::new(1.0, SIZE.gamma_multiply(0.7)), StrokeKind::Inside);
    }
    // Chevron (expansion comes with sizes).
    let chev = Rect::from_min_size(pos2(inner.left() + 4.0, inner.top()), vec2(16.0, inner.height()));
    icons::chevron(p, chev, false, FAINT);
    let glyph = Rect::from_center_size(pos2(chev.right() + 11.0, inner.center().y), vec2(16.0, 14.0));
    icons::folder(p, glyph, icons::tint(crate::model::entry::Category::Folder).gamma_multiply(if e.is_hidden() { 0.5 } else { 1.0 }));
    let count = match w.explorer.counts.get(&e.name) {
        Some(Count::Items(n)) => format!("{} item{}", super::widgets::thousands(*n), if *n == 1 { "" } else { "s" }),
        Some(Count::Unreadable) => "unreadable".into(),
        None => String::new(),
    };
    let right = p.text(pos2(inner.right() - 8.0, inner.center().y), Align2::RIGHT_CENTER, &count, font(12.0), MUTED);
    let name_x = glyph.right() + 8.0;
    let name_w = (right.left() - 12.0 - name_x).max(20.0);
    let color = if e.is_hidden() { MUTED } else { TEXT };
    let g = super::text::one_line(ui, &e.name_lossy(), name_w, font(13.0), color);
    ui.painter().galley(pos2(name_x, inner.center().y - g.size().y / 2.0), g, color);

    let resp = resp.on_hover_ui(|ui| {
        ui.label(egui::RichText::new(e.name_lossy()).color(TEXT));
        if !count.is_empty() {
            ui.label(egui::RichText::new(&count).color(TEXT_2));
        }
        ui.label(egui::RichText::new(format!("Modified {}", super::widgets::date(e.mtime))).color(MUTED));
    });
    let mods = ui.input(|i| Mods { toggle: i.modifiers.command, range: i.modifiers.shift });
    if resp.clicked() {
        w.focus = Pane::Folders;
        if mods.toggle || mods.range {
            w.explorer.click(Pane::Folders, &e.name, mods);
        } else {
            w.explorer.selection.only(Pane::Folders, &e.name);
            w.activate(Pane::Folders, &e.name);
        }
    }
    if resp.secondary_clicked() {
        w.focus = Pane::Folders;
        if !selected {
            w.explorer.selection.only(Pane::Folders, &e.name);
        }
    }
}
