//! Everything that isn't drawing. No egui in here: listing, sizes, search,
//! previews and file jobs run on worker threads and report back over
//! channels; [`explorer::Explorer`] gathers their results for the UI.

pub mod apps;
pub mod complete;
#[cfg(target_os = "linux")]
pub mod dbus;
pub mod entry;
pub mod explorer;
pub mod fallback;
pub mod listing;
pub mod opener;
pub mod ops;
pub mod platform;
pub mod playback;
pub mod remote;
pub mod repertoire;
pub mod search;
pub mod sizes;
pub mod thumbs;
pub mod view;
