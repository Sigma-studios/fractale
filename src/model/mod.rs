//! Everything that isn't drawing. No egui in here: listing, sizes, search,
//! previews and file jobs run on worker threads and report back over
//! channels; [`explorer::Explorer`] gathers their results for the UI.

pub mod complete;
pub mod entry;
pub mod explorer;
pub mod listing;
pub mod platform;
pub mod view;
