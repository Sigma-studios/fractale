//! Small pieces shared by the panes.

use super::theme::*;
use super::PaneScroll;
use crate::model::explorer::Explorer;
use crate::model::view::{Sort, SortBy};
use eframe::egui::{self, vec2, Align, Layout, RichText, Sense};

/// A pane's title row: eyebrow on the left, controls on the right.
pub fn pane_header(ui: &mut egui::Ui, title: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(eyebrow(title, FAINT));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            right(ui);
        });
    });
    ui.add_space(4.0);
}

/// Sort choices as quiet text toggles; the active one shows its direction.
/// Clicking cycles natural → reversed → unsorted. Returns true on change.
/// `choices` are laid out right to left.
pub fn sort_choices(ui: &mut egui::Ui, sort: &mut Sort, choices: &[SortBy]) -> bool {
    let mut changed = false;
    ui.spacing_mut().item_spacing.x = 2.0;
    for &by in choices {
        let active = sort.by == Some(by);
        let color = if active { TEXT } else { MUTED };
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(by.label().to_string(), font(12.0), color));
        let extra = if active { 12.0 } else { 0.0 };
        let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(10.0 + extra, 6.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 5.0, SURFACE_HOVER);
        }
        let text_pos = rect.left_center() + vec2(5.0, -galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley.clone(), color);
        if active {
            let c = egui::pos2(text_pos.x + galley.size().x + 7.0, rect.center().y);
            sort_arrow(ui.painter(), c, sort.reversed, color);
        }
        let tip = match (active, sort.reversed) {
            (false, _) => format!("Sort by {}", by.label()),
            (true, false) => "Click to reverse".into(),
            (true, true) => "Click for the folder's own order".into(),
        };
        if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            sort.click(by);
            changed = true;
        }
    }
    changed
}

/// ▾ for a column's natural direction (largest, newest, A first), ▴ when
/// reversed. Drawn, since not every font has the glyphs.
pub fn sort_arrow(p: &egui::Painter, c: egui::Pos2, reversed: bool, color: egui::Color32) {
    let (k, h) = (3.5, 3.0);
    let pts = if reversed {
        vec![egui::pos2(c.x - k, c.y + h * 0.6), egui::pos2(c.x + k, c.y + h * 0.6), egui::pos2(c.x, c.y - h)]
    } else {
        vec![egui::pos2(c.x - k, c.y - h * 0.6), egui::pos2(c.x + k, c.y - h * 0.6), egui::pos2(c.x, c.y + h)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// 12345 -> "12,345".
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Placeholder text for an empty, loading or unreadable pane.
pub struct Message {
    text: String,
    color: egui::Color32,
}

impl Message {
    pub fn for_pane(ex: &Explorer, count: usize, empty: &str) -> Option<Message> {
        if ex.is_loading() {
            return Some(Message { text: "Loading…".into(), color: MUTED });
        }
        if let Some(err) = &ex.listing.error {
            return Some(Message { text: err.clone(), color: TEXT_2 });
        }
        (count == 0 && ex.is_current()).then(|| Message { text: empty.into(), color: FAINT })
    }

    pub fn show(self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| ui.label(RichText::new(self.text).color(self.color)));
        ui.allocate_space(ui.available_size());
    }
}

/// Where to scroll so `row` (the cursor's row) is visible, given last
/// frame's scroll state. Only when asked to reveal it.
pub fn scroll_target(s: &mut PaneScroll, row: Option<usize>, reveal: bool) -> Option<f32> {
    if let Some(t) = s.target.take() {
        return Some(t);
    }
    let (Some(row), true) = (row, reveal) else { return None };
    if s.row_height <= 0.0 {
        return None;
    }
    let line = (row / s.columns.max(1)) as f32;
    let top = line * s.row_height;
    let bottom = top + s.row_height;
    if top < s.offset {
        Some(top)
    } else if bottom > s.offset + s.viewport {
        Some(bottom - s.viewport)
    } else {
        None
    }
}

/// A modification time, short: "today 14:32", "24 Oct 09:05",
/// "3 Mar 2024".
pub fn date(secs: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (Some(t), Some(now)) = (local(secs), local(now_secs())) else { return String::new() };
    if (t.year, t.yday) == (now.year, now.yday) {
        format!("today {:02}:{:02}", t.hour, t.min)
    } else if t.year == now.year && now.yday - t.yday == 1 {
        format!("yesterday {:02}:{:02}", t.hour, t.min)
    } else if t.year == now.year {
        format!("{} {} {:02}:{:02}", t.mday, MONTHS[t.mon as usize % 12], t.hour, t.min)
    } else {
        format!("{} {} {}", t.mday, MONTHS[t.mon as usize % 12], t.year)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

struct Local {
    year: i32,
    yday: i32,
    mon: i32,
    mday: i32,
    hour: i32,
    min: i32,
}

fn local(secs: i64) -> Option<Local> {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r writes into the zeroed struct we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        Some(Local { year: tm.tm_year + 1900, yday: tm.tm_yday, mon: tm.tm_mon, mday: tm.tm_mday, hour: tm.tm_hour, min: tm.tm_min })
    }
}
