//! The clipboard and drag and drop with other apps (Linux, Wayland), on top
//! of `model::platform::wayland`. Without it (X11, macOS) Fractale's own
//! clipboard and internal drags still work.
//!
//! - Copy and cut put the files on the system clipboard (`text/uri-list`,
//!   and KDE's cut marker), so Dolphin can paste them; paste asks the
//!   system clipboard, so Fractale pastes what Dolphin copied or cut.
//! - Text fields copy and paste text through it too (eframe's own clipboard
//!   is off: two data devices on one seat break drag actions on KWin).
//! - Every drag of items is a system drag: dropped on another app, it's
//!   that app's; dropped back in Fractale, the pointer is fed to egui (winit
//!   sees nothing during it) and the internal drop happens as usual.
//! - Files dropped from other apps land in the folder row under the
//!   pointer, or the open folder.

use super::Window;
use crate::model::explorer::Clip as OwnClip;
use crate::model::ops::Kind;
use crate::model::platform::wayland::{Action, Clip, DataDevice, Event};
use eframe::egui::{self, Pos2};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default)]
pub struct System {
    dd: Option<DataDevice>,
    tried: bool,
    /// Our drag is running (start_drag .. DragSourceDone).
    own_drag: bool,
    /// Where the pointer was during our drag (None after it left).
    hover: Option<Pos2>,
    /// A clipboard request for a text field.
    paste_text: Option<u64>,
    /// A clipboard request for pasting files into a folder.
    paste_files: Option<(u64, PathBuf)>,
    /// The file clipboard last put on the system clipboard.
    sent: Option<OwnClip>,
}

impl System {
    /// First frame: start the data device (Wayland only).
    pub fn start(&mut self, ctx: &egui::Context, frame: &eframe::Frame, w: &mut Window) {
        if self.tried {
            return;
        }
        self.tried = true;
        let c = ctx.clone();
        // SAFETY: the DataDevice lives in the App, which eframe drops before
        // the window and the Wayland connection.
        match unsafe { DataDevice::start_from_window(frame, Arc::new(move || c.request_repaint())) } {
            Ok(dd) => {
                self.dd = Some(dd);
                w.system_clipboard = true;
            }
            Err(e) => {
                if cfg!(target_os = "linux") {
                    eprintln!("fractale: no Wayland data device ({e}); clipboard and drags stay inside Fractale");
                }
            }
        }
    }

    /// Before each pass: the worker's events become input.
    pub fn input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput, w: &mut Window) {
        let Some(dd) = &self.dd else { return };
        // Ctrl+V in a text field asks the system clipboard; the text comes
        // as a paste event a frame or two later.
        if ctx.egui_wants_keyboard_input() {
            let before = raw.events.len();
            raw.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command_only()));
            if raw.events.len() != before {
                self.paste_text = Some(dd.request_clipboard());
            }
        }
        let zoom = ctx.zoom_factor();
        let pos = |x: f64, y: f64| Pos2::new(x as f32 / zoom, y as f32 / zoom);
        let mods = ctx.input(|i| i.modifiers);
        while let Some(ev) = dd.try_recv() {
            match ev {
                Event::DragEnter { x, y, own, .. } => {
                    let p = pos(x, y);
                    // Files may come here: copy unless the other side or
                    // Shift (KWin applies it) says move.
                    dd.set_drop_action(Some(Action::Copy));
                    if own && self.own_drag {
                        self.hover = Some(p);
                        raw.events.push(egui::Event::PointerMoved(p));
                    }
                }
                Event::DragMotion { x, y } if self.own_drag => {
                    let p = pos(x, y);
                    self.hover = Some(p);
                    raw.events.push(egui::Event::PointerMoved(p));
                }
                Event::DragLeave if self.own_drag => {
                    self.hover = None;
                    raw.events.push(egui::Event::PointerGone);
                }
                Event::Dropped { paths, action, x, y, own } => {
                    if action == Action::Ask {
                        dd.resolve_ask(Some(Action::Copy));
                    }
                    // Our own items dropped back on us: egui's drop handles
                    // it (on the release we give it below).
                    if !own && !paths.is_empty() {
                        let p = pos(x, y);
                        let dest = w.drop_rows.iter().find(|(r, _)| r.contains(p)).map(|(_, d)| d.clone()).unwrap_or_else(|| w.explorer.folder.clone());
                        let kind = if action == Action::Move { Kind::Move } else { Kind::Copy };
                        w.explorer.transfer(kind, paths, dest);
                    }
                }
                Event::DragSourceDone { .. } => {
                    self.own_drag = false;
                    // winit never saw the button go up.
                    let pos = self.hover.take();
                    if pos.is_none() {
                        raw.events.push(egui::Event::PointerGone);
                    }
                    raw.events.push(egui::Event::PointerButton { pos: pos.unwrap_or(Pos2::new(-1.0, -1.0)), button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
                }
                Event::Clipboard { id, clip } => {
                    if Some(id) == self.paste_text {
                        self.paste_text = None;
                        match clip {
                            Clip::Text(t) => raw.events.push(egui::Event::Paste(t)),
                            Clip::Files { paths, .. } => raw.events.push(egui::Event::Paste(paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(" "))),
                            Clip::Empty => {}
                        }
                    } else if let Some((want, dest)) = self.paste_files.take() {
                        if want != id {
                            self.paste_files = Some((want, dest));
                            continue;
                        }
                        match clip {
                            Clip::Files { paths, cut } => {
                                let kind = if cut { Kind::Move } else { Kind::Copy };
                                w.explorer.transfer(kind, paths, dest);
                                if cut {
                                    // Cut files are pasted once.
                                    dd.clear_clipboard();
                                    w.explorer.clipboard = None;
                                    self.sent = None;
                                }
                            }
                            Clip::Text(_) => w.explorer.notice = Some(("The clipboard has text, not files".into(), std::time::Instant::now())),
                            // Nothing outside: Fractale's own clipboard.
                            Clip::Empty => {
                                w.explorer.paste(dest);
                            }
                        }
                    }
                }
                Event::SelectionLost => {
                    // Another app copied something: ours is gone.
                    w.explorer.clipboard = None;
                    self.sent = None;
                }
                Event::Error(e) => eprintln!("fractale: data device: {e}"),
                _ => {}
            }
        }
    }

    /// After the pass: what the window asked for goes out.
    pub fn output(&mut self, ctx: &egui::Context, w: &mut Window) {
        let Some(dd) = &self.dd else {
            w.system_drag = None;
            // X11: text through arboard (macOS: through egui).
            if cfg!(target_os = "linux") {
                for t in ctx.output(|o| o.commands.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }).collect::<Vec<_>>()) {
                    crate::model::platform::set_clipboard_text(t);
                }
            }
            return;
        };
        let copied: Vec<String> = ctx.output(|o| o.commands.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }).collect());
        for t in copied {
            dd.set_clipboard_text(t);
            self.sent = None;
        }
        if w.explorer.clipboard != self.sent {
            if let Some(c) = &w.explorer.clipboard {
                dd.set_clipboard_files(c.paths.clone(), c.cut);
            }
            self.sent = w.explorer.clipboard.clone();
        }
        if let Some(paths) = w.system_drag.take() {
            dd.start_drag(paths, None);
            self.own_drag = true;
        }
        if let Some(dest) = w.paste_request.take() {
            self.paste_files = Some((dd.request_clipboard(), dest));
        }
    }
}
