//! Icons drawn with the painter in the theme's style, so Fractale looks the
//! same on any desktop: a page per file type, with the extension written on
//! it and a small mark for the broad category.

use super::theme::*;
use crate::model::entry::Category;
use eframe::egui::{pos2, vec2, Align2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

/// A muted accent per category: identity, not status.
pub fn tint(c: Category) -> Color32 {
    match c {
        Category::Folder => hex(0x6b8fb5),
        Category::Image => hex(0x5f9e8a),
        Category::Video => hex(0x8a7bc4),
        Category::Audio => hex(0xb07aa1),
        Category::Archive => hex(0xb59a5f),
        Category::Document => hex(0xa3a39a),
        Category::Code => hex(0x6f9fc0),
        Category::Executable => hex(0xb57f5f),
        Category::Other => hex(0x7d7c76),
    }
}

/// A file icon filling `rect` (square-ish): a page with a folded corner,
/// the extension on it, and the category mark.
pub fn file(p: &Painter, rect: Rect, category: Category, ext: &str, broken: bool) {
    let s = rect.width().min(rect.height());
    let w = s * 0.72;
    let h = s * 0.9;
    let page = Rect::from_center_size(rect.center(), vec2(w, h));
    let fold = w * 0.26;
    let c = tint(category);
    let fill = if broken { SURFACE } else { RAISED };
    let pts = vec![
        page.left_top(),
        pos2(page.right() - fold, page.top()),
        pos2(page.right(), page.top() + fold),
        page.right_bottom(),
        page.left_bottom(),
    ];
    let stroke = if broken { Stroke::new(1.0, FAINT) } else { Stroke::new(1.0, BORDER.gamma_multiply(1.6)) };
    p.add(Shape::convex_polygon(pts, fill, stroke));
    // The fold.
    p.add(Shape::convex_polygon(
        vec![pos2(page.right() - fold, page.top()), pos2(page.right(), page.top() + fold), pos2(page.right() - fold, page.top() + fold)],
        c.gamma_multiply(0.45),
        Stroke::NONE,
    ));
    // Category band at the bottom.
    let band = Rect::from_min_max(pos2(page.left(), page.bottom() - h * 0.07), page.right_bottom());
    p.rect_filled(band, CornerRadius::ZERO, c.gamma_multiply(if broken { 0.3 } else { 0.85 }));
    // The extension, or the category mark when there's none.
    let label: String = ext.chars().take(5).collect::<String>().to_uppercase();
    if !label.is_empty() && s >= 40.0 {
        let size = (s * 0.16).clamp(8.0, 22.0) * if label.len() > 3 { 0.82 } else { 1.0 };
        p.text(page.center() + vec2(0.0, h * 0.05), Align2::CENTER_CENTER, label, strong(size), if broken { FAINT } else { c });
    } else {
        mark(p, Rect::from_center_size(page.center() + vec2(0.0, h * 0.04), vec2(w * 0.5, w * 0.5)), category, c);
    }
    if broken {
        // A crack across the page: a dangling link.
        let a = pos2(page.left() + w * 0.15, page.top() + h * 0.3);
        let pts = [a, a + vec2(w * 0.25, h * 0.12), a + vec2(w * 0.4, h * 0.02), a + vec2(w * 0.7, h * 0.16)];
        p.add(Shape::line(pts.to_vec(), Stroke::new(1.5, CRITICAL.gamma_multiply(0.8))));
    }
}

/// A small glyph for a category.
pub fn mark(p: &Painter, r: Rect, category: Category, c: Color32) {
    let st = Stroke::new((r.width() * 0.09).max(1.0), c);
    let center = r.center();
    match category {
        Category::Image => {
            // A mountain and a sun.
            let base = r.bottom() - r.height() * 0.15;
            p.add(Shape::convex_polygon(
                vec![pos2(r.left(), base), pos2(r.left() + r.width() * 0.35, r.top() + r.height() * 0.35), pos2(r.left() + r.width() * 0.7, base)],
                c,
                Stroke::NONE,
            ));
            p.circle_filled(pos2(r.right() - r.width() * 0.22, r.top() + r.height() * 0.28), r.width() * 0.12, c);
        }
        Category::Video => {
            p.add(Shape::convex_polygon(
                vec![pos2(center.x - r.width() * 0.25, r.top() + r.height() * 0.15), pos2(center.x + r.width() * 0.35, center.y), pos2(center.x - r.width() * 0.25, r.bottom() - r.height() * 0.15)],
                c,
                Stroke::NONE,
            ));
        }
        Category::Audio => {
            // A waveform.
            let n = 7;
            for i in 0..n {
                let x = r.left() + r.width() * (i as f32 + 0.5) / n as f32;
                let amp = [0.25, 0.6, 0.9, 0.5, 0.75, 0.35, 0.2][i] * r.height() * 0.45;
                p.line_segment([pos2(x, center.y - amp), pos2(x, center.y + amp)], st);
            }
        }
        Category::Archive => {
            for i in 0..4 {
                let y = r.top() + r.height() * (0.15 + i as f32 * 0.2);
                p.rect_filled(Rect::from_center_size(pos2(center.x + if i % 2 == 0 { -2.0 } else { 2.0 }, y), vec2(r.width() * 0.18, r.height() * 0.1)), 1.0, c);
            }
        }
        Category::Code => {
            let k = r.width() * 0.22;
            p.add(Shape::line(vec![pos2(center.x - k * 0.5, center.y - k), pos2(center.x - k * 1.4, center.y), pos2(center.x - k * 0.5, center.y + k)], st));
            p.add(Shape::line(vec![pos2(center.x + k * 0.5, center.y - k), pos2(center.x + k * 1.4, center.y), pos2(center.x + k * 0.5, center.y + k)], st));
        }
        Category::Executable => {
            p.add(Shape::line(vec![pos2(r.left() + r.width() * 0.2, r.top() + r.height() * 0.3), pos2(center.x, center.y), pos2(r.left() + r.width() * 0.2, r.bottom() - r.height() * 0.3)], st));
            p.line_segment([pos2(center.x + 2.0, r.bottom() - r.height() * 0.3), pos2(r.right() - r.width() * 0.15, r.bottom() - r.height() * 0.3)], st);
        }
        Category::Folder => folder(p, r, c),
        Category::Document | Category::Other => {
            for i in 0..4 {
                let y = r.top() + r.height() * (0.2 + i as f32 * 0.2);
                let len = if i == 3 { 0.5 } else { 0.85 };
                p.line_segment([pos2(r.left() + r.width() * 0.08, y), pos2(r.left() + r.width() * len, y)], st);
            }
        }
    }
}

/// A folder glyph.
pub fn folder(p: &Painter, r: Rect, c: Color32) {
    let tab = Rect::from_min_size(r.left_top() + vec2(0.0, r.height() * 0.12), vec2(r.width() * 0.42, r.height() * 0.2));
    p.rect_filled(tab, CornerRadius::same(2), c.gamma_multiply(0.8));
    let body = Rect::from_min_max(pos2(r.left(), r.top() + r.height() * 0.24), pos2(r.right(), r.bottom() - r.height() * 0.08));
    p.rect_filled(body, CornerRadius::same(3), c);
}

/// A chevron pointing right (or down when `open`), centred in `r`.
pub fn chevron(p: &Painter, r: Rect, open: bool, color: Color32) {
    let c = r.center();
    let k = r.width().min(r.height()) * 0.22;
    let pts = if open {
        vec![pos2(c.x - k, c.y - k * 0.5), pos2(c.x, c.y + k * 0.5), pos2(c.x + k, c.y - k * 0.5)]
    } else {
        vec![pos2(c.x - k * 0.5, c.y - k), pos2(c.x + k * 0.5, c.y), pos2(c.x - k * 0.5, c.y + k)]
    };
    p.add(Shape::line(pts, Stroke::new(1.6, color)));
}

#[derive(Clone, Copy)]
pub enum Arrow {
    Left,
    Right,
    Up,
}

/// A navigation arrow (back, forward, up).
pub fn arrow(p: &Painter, r: Rect, dir: Arrow, color: Color32) {
    let c = r.center();
    let k = r.width().min(r.height()) * 0.26;
    let st = Stroke::new(1.8, color);
    let (tip, a, b, tail): (Pos2, Pos2, Pos2, Pos2) = match dir {
        Arrow::Left => (pos2(c.x - k, c.y), pos2(c.x - k * 0.2, c.y - k * 0.8), pos2(c.x - k * 0.2, c.y + k * 0.8), pos2(c.x + k, c.y)),
        Arrow::Right => (pos2(c.x + k, c.y), pos2(c.x + k * 0.2, c.y - k * 0.8), pos2(c.x + k * 0.2, c.y + k * 0.8), pos2(c.x - k, c.y)),
        Arrow::Up => (pos2(c.x, c.y - k), pos2(c.x - k * 0.8, c.y - k * 0.2), pos2(c.x + k * 0.8, c.y - k * 0.2), pos2(c.x, c.y + k)),
    };
    p.line_segment([tail, tip], st);
    p.add(Shape::line(vec![a, tip, b], st));
}

/// A magnifier, for the search field.
pub fn magnifier(p: &Painter, r: Rect, color: Color32) {
    let k = r.width().min(r.height());
    let c = r.center() - vec2(k * 0.08, k * 0.08);
    p.circle_stroke(c, k * 0.26, Stroke::new(1.5, color));
    p.line_segment([c + vec2(k * 0.19, k * 0.19), c + vec2(k * 0.38, k * 0.38)], Stroke::new(1.8, color));
}

/// An eye: almond outline and pupil ("look inside").
pub fn eye(p: &Painter, r: Rect, color: Color32) {
    let (c, hw, hh) = (r.center(), r.width().min(r.height() * 1.6) * 0.42, r.height() * 0.3);
    let n = 12;
    // Upper and lower lids as arcs through the corners.
    let lid = |sign: f32| -> Vec<eframe::egui::Pos2> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let x = -hw + 2.0 * hw * t;
                c + vec2(x, sign * hh * (1.0 - (x / hw).powi(2)))
            })
            .collect()
    };
    p.add(eframe::egui::Shape::line(lid(-1.0), Stroke::new(1.4, color)));
    p.add(eframe::egui::Shape::line(lid(1.0), Stroke::new(1.4, color)));
    p.circle_filled(c, hh * 0.62, color);
}

/// The grid/list toggle glyphs.
pub fn grid_glyph(p: &Painter, r: Rect, color: Color32) {
    let k = r.width().min(r.height()) * 0.22;
    for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        p.rect_filled(Rect::from_center_size(r.center() + vec2(dx * k * 0.6, dy * k * 0.6), vec2(k * 0.9, k * 0.9)), 1.0, color);
    }
}

pub fn list_glyph(p: &Painter, r: Rect, color: Color32) {
    let k = r.width().min(r.height()) * 0.24;
    for i in -1..=1 {
        let y = r.center().y + i as f32 * k * 0.8;
        p.line_segment([pos2(r.center().x - k, y), pos2(r.center().x + k, y)], Stroke::new(1.6, color));
    }
}

/// A small chain-link badge for symlinks, at the bottom-left of `r`.
pub fn link_badge(p: &Painter, r: Rect, broken: bool) {
    let s = (r.width() * 0.26).clamp(10.0, 22.0);
    let b = Rect::from_min_size(pos2(r.left() + 2.0, r.bottom() - s - 2.0), vec2(s, s));
    p.rect_filled(b, CornerRadius::same(4), BG);
    p.rect_stroke(b, CornerRadius::same(4), Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let c = if broken { CRITICAL } else { TEXT_2 };
    let k = s * 0.18;
    p.circle_stroke(b.center() - vec2(k * 0.7, -k * 0.7), k, Stroke::new(1.2, c));
    p.circle_stroke(b.center() + vec2(k * 0.7, -k * 0.7), k, Stroke::new(1.2, c));
}
