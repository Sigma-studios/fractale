//! Back, forward, up; the breadcrumb (a text field on click or Ctrl+L, with
//! completion); the search field.

use super::icons::{self, Arrow};
use super::theme::*;
use super::Window;
use crate::model::complete::Completer;
use eframe::egui::{self, vec2, Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke, StrokeKind};
use std::path::{Component, Path, PathBuf};

#[derive(Default)]
pub struct TopBar {
    /// Some while the breadcrumb is a text field.
    pub path_text: Option<String>,
    focus_path: bool,
    pub search: String,
    pub focus_search: bool,
    completer: Option<Completer>,
    /// Completion candidates for the current text.
    suggestions: Vec<String>,
}

impl TopBar {
    pub fn edit_path(&mut self, folder: &Path) {
        let mut t = folder.display().to_string();
        if !t.ends_with('/') {
            t.push('/');
        }
        self.path_text = Some(t);
        self.focus_path = true;
    }

    pub fn cancel(&mut self) {
        self.path_text = None;
        self.suggestions.clear();
    }
}

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let (can_back, can_forward) = (w.explorer.history.can_back(), w.explorer.history.can_forward());
        if nav_button(ui, Arrow::Left, can_back, "Back (Alt+Left)").clicked() {
            w.explorer.back();
        }
        if nav_button(ui, Arrow::Right, can_forward, "Forward (Alt+Right)").clicked() {
            w.explorer.forward();
        }
        let has_parent = w.explorer.folder.parent().is_some();
        if nav_button(ui, Arrow::Up, has_parent, "Up (Alt+Up)").clicked() {
            w.explorer.up();
        }
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            search_field(ui, w);
            ui.add_space(12.0);
            let rest = ui.available_rect_before_wrap();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rest).layout(Layout::left_to_right(Align::Center)));
            if w.topbar.path_text.is_some() {
                path_field(&mut child, w);
            } else {
                breadcrumb(&mut child, w, rest);
            }
        });
    });
}

fn nav_button(ui: &mut egui::Ui, arrow: Arrow, enabled: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), SURFACE_HOVER);
    }
    icons::arrow(p, rect, arrow, if enabled { if resp.hovered() { TEXT } else { TEXT_2 } } else { FAINT });
    let name = tip.split(' ').next().unwrap_or(tip).to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &name));
    let resp = resp.on_hover_text(tip);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// The folder's path as clickable segments; the empty space after them
/// turns the whole thing into a text field.
fn breadcrumb(ui: &mut egui::Ui, w: &mut Window, area: Rect) {
    let folder = w.explorer.folder.clone();
    // Clicking the empty part of the bar edits the path.
    let bg = ui.interact(area, ui.scope_id().with("crumb-bg"), Sense::click());
    if bg.clicked() {
        w.topbar.edit_path(&folder);
        return;
    }
    ui.spacing_mut().item_spacing.x = 0.0;
    let mut acc = PathBuf::new();
    let parts: Vec<(String, PathBuf)> = folder
        .components()
        .map(|c| {
            acc.push(c.as_os_str());
            let label = match c {
                Component::RootDir => "/".to_string(),
                c => c.as_os_str().to_string_lossy().into_owned(),
            };
            (label, acc.clone())
        })
        .collect();
    // Long paths keep their end visible: drop leading segments into "…".
    let max_w = area.width() - 40.0;
    let widths: Vec<f32> = parts.iter().map(|(l, _)| text_width(ui, l) + 22.0).collect();
    let mut first = 0;
    while first + 1 < parts.len() && widths[first..].iter().sum::<f32>() > max_w {
        first += 1;
    }
    if first > 0 {
        ui.label(RichText::new("…").color(MUTED));
        separator(ui);
    }
    let last = parts.len().saturating_sub(1);
    for (i, (label, path)) in parts.iter().enumerate().skip(first) {
        let is_last = i == last;
        let font = if is_last { strong(13.0) } else { font(13.0) };
        let color = if is_last { TEXT } else { TEXT_2 };
        let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), font, color));
        let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(12.0, 10.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, format!("crumb {label}")));
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(5), SURFACE_HOVER);
        }
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !is_last {
            w.explorer.open(path.clone());
        }
        if !is_last && label != "/" {
            separator(ui);
        }
    }
}

fn separator(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
    icons::chevron(ui.painter(), r, false, FAINT);
}

fn text_width(ui: &egui::Ui, s: &str) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font(13.0), TEXT).size().x)
}

/// The path as text. Enter opens it, Escape cancels, Tab completes.
fn path_field(ui: &mut egui::Ui, w: &mut Window) {
    let completer = w.topbar.completer.get_or_insert_with(Completer::spawn);
    if let Some(s) = completer.poll() {
        w.topbar.suggestions = s;
    }
    let Some(text) = w.topbar.path_text.as_mut() else { return };
    let before = text.clone();
    let tab = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    let edit = egui::TextEdit::singleline(text)
        .desired_width(ui.available_width() - 8.0)
        .font(font(13.0))
        .margin(egui::Margin::symmetric(8, 5))
        .id(egui::Id::unique("path-field"));
    let out = edit.show(ui);
    let resp = out.response;
    if w.topbar.focus_path {
        resp.request_focus();
        // Cursor at the end.
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
            let end = egui::text::CCursor::new(text.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), resp.id);
        }
        w.topbar.focus_path = false;
    }
    if tab {
        if let Some(common) = common_prefix(&w.topbar.suggestions) {
            if common.len() > text.len() {
                *text = common;
                if w.topbar.suggestions.len() == 1 && !text.ends_with('/') {
                    text.push('/');
                }
            }
        }
        resp.request_focus();
        move_cursor_to_end(ui, resp.id, text);
    }
    if *text != before || w.topbar.suggestions.is_empty() {
        completer.ask(text.clone());
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if enter {
        let target = PathBuf::from(expand_tilde(text.trim()));
        w.topbar.cancel();
        if target.is_absolute() {
            w.explorer.open(target);
        }
        return;
    }
    if resp.lost_focus() && !tab {
        w.topbar.cancel();
        return;
    }
    // Suggestions under the field.
    if !w.topbar.suggestions.is_empty() && resp.has_focus() {
        let pick = suggestions_popup(ui, resp.rect, &w.topbar.suggestions);
        if let Some(s) = pick {
            w.topbar.path_text = Some(s);
            w.topbar.focus_path = true;
        }
    }
}

fn move_cursor_to_end(ui: &egui::Ui, id: egui::Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
        let end = egui::text::CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ui.ctx(), id);
    }
}

fn suggestions_popup(ui: &egui::Ui, field: Rect, items: &[String]) -> Option<String> {
    let mut picked = None;
    egui::Area::new(egui::Id::unique("path-suggestions"))
        .fixed_pos(field.left_bottom() + vec2(0.0, 4.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(field.width().min(520.0));
                for s in items.iter().take(12) {
                    let name = Path::new(s.trim_end_matches('/')).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.selectable_label(false, RichText::new(name).color(TEXT_2)).clicked() {
                        picked = Some(format!("{}/", s.trim_end_matches('/')));
                    }
                }
                if items.len() > 12 {
                    ui.label(RichText::new(format!("{} more…", items.len() - 12)).color(MUTED));
                }
            });
        });
    picked
}

fn common_prefix(items: &[String]) -> Option<String> {
    let first = items.first()?;
    let mut end = first.len();
    for s in &items[1..] {
        end = end.min(first.bytes().zip(s.bytes()).take_while(|(a, b)| a == b).count());
    }
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    Some(first[..end].to_string())
}

pub fn expand_tilde(s: &str) -> String {
    match (s.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => s.to_string(),
    }
}

fn search_field(ui: &mut egui::Ui, w: &mut Window) {
    let width = 280.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    let p = child.painter().clone();
    p.rect_filled(rect, CornerRadius::same(7), SURFACE);
    p.rect_stroke(rect, CornerRadius::same(7), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let (glyph, _) = child.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
    icons::magnifier(&p, glyph, MUTED);
    let resp = child.add(
        egui::TextEdit::singleline(&mut w.topbar.search)
            .frame(egui::Frame::NONE)
            .hint_text(RichText::new("Search here…").color(FAINT))
            .desired_width(width - 40.0)
            .id(egui::Id::unique("search-field")),
    );
    if w.topbar.focus_search {
        resp.request_focus();
        w.topbar.focus_search = false;
    }
}
