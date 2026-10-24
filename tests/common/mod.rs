//! Shared helpers for UI tests.
#![allow(dead_code)]

use egui_kittest::{Harness, HarnessBuilder};

/// A harness with Fractale's theme (bundled fonts only, so snapshots don't
/// depend on the machine).
pub fn builder() -> HarnessBuilder {
    Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_context_setup(fractale::ui::theme::install_for_tests)
}
