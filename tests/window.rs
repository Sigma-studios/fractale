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

#[test]
fn ctrl_n_asks_for_a_new_window_here() {
    use egui::{Key, Modifiers};
    let s = common::Scratch::new("new-window");
    let mut h = common::window(&s.0);
    common::listed(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::N);
    h.run();
    let w = h.state();
    assert_eq!(w.new_windows.len(), 1);
    assert_eq!(w.new_windows[0].show, fractale::model::remote::Show { folder: s.0.clone(), select: Vec::new() });
    assert_eq!(w.new_windows[0].token, None);
}
