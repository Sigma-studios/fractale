//! The bottom strip: where the sizes come from, the filesystem's space,
//! and what's selected.

use super::theme::*;
use super::Window;
use crate::model::sizes::{RepertoireState, SourceState};
use eframe::egui::{self, Align, Layout, RichText};

pub fn show(ui: &mut egui::Ui, w: &mut Window) {
    ui.horizontal(|ui| {
        let ex = &w.explorer;
        let (color, label, tip) = source_badge(&ex.source);
        super::widgets::status_badge(ui, color, &label).on_hover_text(tip);
        ui.add_space(6.0);
        if let Some((total, used)) = ex.listing.space {
            ui.label(RichText::new(format!("{} of {} used", bytes(used as f64), bytes(total as f64))).color(MUTED));
        }
        // What just failed (renaming, opening…), for a few seconds.
        if let Some((text, at)) = &ex.notice {
            let left = std::time::Duration::from_secs(8).saturating_sub(at.elapsed());
            if !left.is_zero() {
                ui.add_space(10.0);
                ui.label(RichText::new(text).color(SERIOUS));
                ui.ctx().request_repaint_after(left);
            }
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

/// Where the sizes come from, as a status dot and its label.
fn source_badge(s: &SourceState) -> (egui::Color32, String, &'static str) {
    let pct = |p: f32| (p * 100.0).round() as u32;
    match s {
        SourceState::Repertoire(RepertoireState::Live) => (GOOD, "live".into(), "Sizes follow every change as it happens (répertoire)"),
        SourceState::Repertoire(RepertoireState::Static) => (MUTED, "static".into(), "répertoire measured this, but isn't watching for changes (not running as root?)"),
        SourceState::Repertoire(RepertoireState::Reconciling { progress }) => {
            (MUTED, format!("reconciling {}%", pct(*progress)), "répertoire is checking its saved sizes against the disk")
        }
        SourceState::Repertoire(RepertoireState::Scanning { progress }) => (MUTED, format!("scanning {}%", pct(*progress)), "répertoire is measuring this disk for the first time"),
        SourceState::Repertoire(RepertoireState::Stale { progress }) => {
            (WARNING, format!("stale {}%", pct(*progress)), "répertoire missed some changes and is measuring again; sizes may be off")
        }
        SourceState::OwnScan { busy: true } => (MUTED, "own scan…".into(), "répertoire isn't running: Fractale measures folders itself (not live below this folder)"),
        SourceState::OwnScan { busy: false } => (MUTED, "own scan".into(), "répertoire isn't running: Fractale measures folders itself (not live below this folder)"),
    }
}
