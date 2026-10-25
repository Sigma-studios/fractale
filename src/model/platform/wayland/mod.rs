//! Wayland data-device worker: MIME clipboard, drops from other apps, drags to
//! other apps. winit 0.30 has none of these, so a worker thread opens its own
//! event queue on winit's `wl_display`, with its own `wl_seat`, `wl_pointer`,
//! `wl_keyboard` (for input serials) and `wl_data_device`.
//!
//! Run eframe WITHOUT its `clipboard` feature: a second data device on the
//! same seat (smithay-clipboard) breaks DnD actions on KWin. This worker serves
//! the plain-text clipboard as well.
//!
//! Everything is `std::thread`; nothing here blocks the UI thread except
//! [`DataDevice::start`], which waits for the initial registry roundtrip.
//! Errors arrive as [`Event::Error`]; nothing panics on compositor input.
//!
//! On non-Linux targets this compiles to a stub whose `start` returns `Err`.
//!
//! # Integration with eframe 0.37 (see `examples/demo.rs`)
//!
//! - `NativeOptions { clipboard_shortcuts: false, .. }`: Ctrl+C/X/V then reach
//!   the app as plain `Key` events instead of egui-winit's in-process fallback
//!   clipboard (which is all it has without the `clipboard` feature).
//! - Start in `App::logic` (or `ui`) on the first frame:
//!   `unsafe { DataDevice::start_from_window(frame, Arc::new(move || ctx.request_repaint())) }`.
//!   `Err` means not Wayland (X11, XWayland) or no data device: fall back.
//!   Keep the `DataDevice` in the `App`; eframe drops the app before the
//!   window and the connection, and dropping it joins the worker.
//! - Drain events in `App::raw_input_hook` (runs before each pass), so the
//!   app can edit the `RawInput`:
//!   - during our own drag (`start_drag` .. `DragSourceDone`), push
//!     `egui::Event::PointerMoved(pos)` for each `DragEnter`/`DragMotion` and
//!     `PointerGone` on `DragLeave`; winit gets no pointer events then;
//!   - on `DragSourceDone`, push a `PointerButton { pressed: false }` for the
//!     primary button, or egui stays in "dragging";
//!   - turn Ctrl+C/X `Key` events into `Event::Copy`/`Event::Cut` when a text
//!     field has focus (files otherwise); for Ctrl+V call `request_clipboard`
//!     and push `Event::Paste(text)` when the matching `Event::Clipboard` comes.
//! - After the UI pass, forward `OutputCommand::CopyText` from
//!   `ctx.output(|o| o.commands)` to [`DataDevice::set_clipboard_text`].
//! - Positions are `wl_surface` logical pixels: egui points = value / `ctx.zoom_factor()`.
//! - On each `DragEnter`/`DragMotion`, call `set_drop_action` / `set_drop_actions`
//!   for whatever is under the pointer (the worker resets to copy on enter).

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod uri;
#[cfg(target_os = "linux")]
mod worker;

/// A drag-and-drop action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Copy,
    Move,
    /// The user is to choose (e.g. a menu). Only negotiated if the target
    /// asked for it with `set_drop_action(Some(Action::Ask))`.
    Ask,
}

/// A set of actions (what a drag source allows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    pub copy: bool,
    pub move_: bool,
    pub ask: bool,
}

impl Actions {
    pub fn contains(self, a: Action) -> bool {
        match a {
            Action::Copy => self.copy,
            Action::Move => self.move_,
            Action::Ask => self.ask,
        }
    }
}

/// Clipboard contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    Empty,
    /// Local files from `text/uri-list`. `cut` comes from
    /// `application/x-kde-cutselection` (KDE) or `x-special/gnome-copied-files`.
    Files { paths: Vec<PathBuf>, cut: bool },
    Text(String),
}

/// Image shown under the cursor during an outgoing drag.
#[derive(Clone, Debug)]
pub struct DragIcon {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Point of the icon that sits under the cursor hotspot.
    pub hotspot: (i32, i32),
}

/// What the worker reports. Coordinates are `wl_surface`-local logical pixels
/// (divide by `egui::Context::zoom_factor()` to get egui points).
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A drag entered our window. `own` = it is our own drag (from
    /// [`DataDevice::start_drag`]). The preferred drop action is reset to
    /// `Copy` on every enter; change it with [`DataDevice::set_drop_action`].
    DragEnter { x: f64, y: f64, mime: Vec<String>, source_actions: Actions, own: bool },
    /// The drag moved over our window. During our own drag this is also the
    /// only pointer position the app gets (winit sees nothing then).
    DragMotion { x: f64, y: f64 },
    /// The drag left our window without dropping (or the drop was refused).
    DragLeave,
    /// Files were dropped on us. Already `finish`ed unless `action` is `Ask`,
    /// in which case call [`DataDevice::resolve_ask`]. On `Move` the source
    /// (or for `own`, the app) moves the files; the data device never does.
    Dropped { paths: Vec<PathBuf>, action: Action, x: f64, y: f64, own: bool },
    /// Live feedback for our outgoing drag: whether the target under the
    /// cursor accepts it, and with which action.
    DragSourceFeedback { accepted: bool, action: Option<Action> },
    /// The user released the button over a target that accepted.
    DragSourceDropped,
    /// Our outgoing drag is over. `Some(action)`: the target finished with that
    /// action (on `Move`, the target moved the files; don't delete them).
    /// `None`: cancelled or refused. winit missed the button release, so the
    /// app must inject one into egui (see the demo).
    DragSourceDone { action: Option<Action> },
    /// Answer to [`DataDevice::request_clipboard`] with the same id.
    Clipboard { id: u64, clip: Clip },
    /// Another client took the clipboard we had set.
    SelectionLost,
    /// Something failed. The worker keeps running unless the message says it stopped.
    Error(String),
}

#[cfg(target_os = "linux")]
pub use worker::DataDevice;

#[cfg(not(target_os = "linux"))]
mod stub {
    use super::*;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;

    /// Stub: there is no Wayland here.
    pub struct DataDevice {
        events: Receiver<Event>,
    }

    impl DataDevice {
        /// # Safety
        /// Trivially safe on this platform; always returns `Err`.
        pub unsafe fn start(
            _display: NonNull<c_void>,
            _surface: NonNull<c_void>,
            _wake: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<DataDevice, String> {
            Err("the Wayland data device is only available on Linux".into())
        }
        /// # Safety
        /// Trivially safe on this platform; always returns `Err`.
        pub unsafe fn start_from_window<W>(_window: &W, _wake: Arc<dyn Fn() + Send + Sync>) -> Result<DataDevice, String>
        where
            W: raw_window_handle::HasDisplayHandle + raw_window_handle::HasWindowHandle + ?Sized,
        {
            Err("the Wayland data device is only available on Linux".into())
        }
        pub fn set_clipboard_files(&self, _paths: Vec<PathBuf>, _cut: bool) {}
        pub fn set_clipboard_text(&self, _text: String) {}
        pub fn clear_clipboard(&self) {}
        pub fn request_clipboard(&self) -> u64 {
            0
        }
        pub fn start_drag(&self, _paths: Vec<PathBuf>, _icon: Option<DragIcon>) {}
        pub fn set_drop_action(&self, _action: Option<Action>) {}
        pub fn set_drop_actions(&self, _allowed: Actions, _preferred: Action) {}
        pub fn resolve_ask(&self, _action: Option<Action>) {}
        pub fn events(&self) -> &Receiver<Event> {
            &self.events
        }
        pub fn try_recv(&self) -> Option<Event> {
            self.events.try_recv().ok()
        }
        pub fn is_running(&self) -> bool {
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use stub::DataDevice;
