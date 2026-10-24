//! Laying out file names: wrapped on up to two lines, and when that's not
//! enough, cut in the middle so the start and the extension stay visible
//! (`a_very_long_na…_name.tar.gz`). Measuring takes a few layouts per name,
//! so results are cached per name and width.

use eframe::egui::{self, text::LayoutJob, Color32, FontId, Galley};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct NameCache {
    fitted: HashMap<(String, u32, u8), String>,
}

impl NameCache {
    /// `name` fitted into `lines` lines of `width`, as a galley.
    pub fn layout(&mut self, ui: &egui::Ui, name: &str, width: f32, lines: u8, font: FontId, color: Color32) -> Arc<Galley> {
        if self.fitted.len() > 20_000 {
            self.fitted.clear();
        }
        let key = (name.to_string(), width as u32, lines);
        let text = match self.fitted.get(&key) {
            Some(t) => t.clone(),
            None => {
                let t = fit(ui, name, width, lines, &font);
                self.fitted.insert(key, t.clone());
                t
            }
        };
        ui.fonts_mut(|f| f.layout_job(job(&text, width, lines, font, color)))
    }
}

fn job(text: &str, width: f32, lines: u8, font: FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::simple(text.to_string(), font, color, width);
    job.wrap.max_rows = lines as usize;
    job.wrap.break_anywhere = true;
    job.halign = egui::Align::Center;
    job
}

fn rows(ui: &egui::Ui, text: &str, width: f32, font: &FontId) -> usize {
    let mut j = LayoutJob::simple(text.to_string(), font.clone(), Color32::WHITE, width);
    j.wrap.break_anywhere = true;
    ui.fonts_mut(|f| f.layout_job(j)).rows.len()
}

fn fit(ui: &egui::Ui, name: &str, width: f32, lines: u8, font: &FontId) -> String {
    if rows(ui, name, width, font) <= lines as usize {
        return name.to_string();
    }
    let chars: Vec<char> = name.chars().collect();
    // Keep the end (extension and a bit more), shorten the start.
    let tail = (chars.len() / 3).clamp(4, 12).min(chars.len().saturating_sub(1));
    let end: String = chars[chars.len() - tail..].iter().collect();
    let (mut lo, mut hi) = (0usize, chars.len() - tail);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate = format!("{}…{}", chars[..mid].iter().collect::<String>(), end);
        if rows(ui, &candidate, width, font) <= lines as usize {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}…{}", chars[..lo].iter().collect::<String>(), end)
}

/// One line, cut at the end with an ellipsis.
pub fn one_line(ui: &egui::Ui, text: &str, width: f32, font: FontId, color: Color32) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_string(), font, color, width.max(1.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    ui.fonts_mut(|f| f.layout_job(job))
}
