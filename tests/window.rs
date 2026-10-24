//! The window as a whole, driven headless.

mod common;

use egui_kittest::kittest::Queryable;
use fractale::ui::{Start, Window};

#[test]
fn opens_with_two_panes() {
    let mut window = Window::new(Start { folder: std::env::temp_dir(), select: None });
    let mut harness = common::builder::<()>().build_ui(|ui| window.ui(ui));
    harness.run();
    harness.get_by_label("Back");
    harness.get_by_label("Up");
}
