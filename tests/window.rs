//! The window as a whole, driven headless.

mod common;

use egui_kittest::kittest::Queryable;

#[test]
fn opens_with_two_panes() {
    let s = common::Scratch::new("window");
    let mut h = common::window(&s.0);
    common::listed(&mut h);
    h.get_by_label("Back");
    h.get_by_label("Up");
}
