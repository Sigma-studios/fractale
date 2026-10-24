//! The bottom strip: where the sizes come from, the filesystem's space,
//! and what's selected.

use super::theme::*;
use super::Window;
use eframe::egui::{self, Align, Layout, RichText};

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    ui.horizontal(|ui| {
        let ex = &w.explorer;
        if let Some((total, used)) = ex.listing.space {
            ui.label(RichText::new(format!("{} of {} used", bytes(used as f64), bytes(total as f64))).color(MUTED));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let selected = ex.selection.items.len();
            let text = if selected > 0 {
                let total: u64 = ex.listing.entries.iter().filter(|e| ex.selection.is_selected(&e.name) && !e.is_folder()).map(|e| e.bytes).sum();
                let folders = ex.listing.entries.iter().filter(|e| ex.selection.is_selected(&e.name) && e.is_folder()).count();
                let mut t = format!("{selected} selected");
                if total > 0 {
                    t.push_str(&format!(" · {}", bytes(total as f64)));
                }
                if folders > 0 && total > 0 {
                    t.push_str(" in files");
                }
                t
            } else {
                format!("{} folders · {} files", super::widgets::thousands(ex.folders.len() as u64), super::widgets::thousands(ex.files.len() as u64))
            };
            ui.label(RichText::new(text).color(TEXT_2));
        });
    });
}
