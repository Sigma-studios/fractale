//! Fractale: a file explorer for finding what uses disk space.
//!
//! `model` knows nothing about the UI: it lists folders, sizes them (from
//! répertoire or by itself), searches, makes previews and runs file jobs,
//! all on worker threads. `ui` renders its state and sends it commands; the
//! UI thread never touches the disk.

pub mod model;
pub mod ui;
