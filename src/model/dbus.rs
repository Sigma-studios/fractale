//! The desktop's file manager interface over the session bus: browsers' and
//! other apps' "show in folder".
//!
//! Every Fractale queues for `org.freedesktop.FileManager1`: one gets it when
//! no other file manager holds it, or when the one holding it quits (taking
//! it from Dolphin would be rude; which file manager is the default is the
//! desktop's choice). Requests open new windows, like Dolphin's and
//! Nautilus's do.

use super::remote::{path_of_uri, NewWindow, Remote, Show, Wake};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;

const FILE_MANAGER: &str = "org.freedesktop.FileManager1";
const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";

#[derive(Clone)]
struct Inbox {
    tx: Arc<Mutex<Sender<NewWindow>>>,
    wake: Arc<OnceLock<Wake>>,
}

impl Inbox {
    /// One window per `Show`; the first gets the caller's activation token
    /// (it's single use).
    fn deliver(&self, shows: Vec<Show>, startup_id: String) {
        if shows.is_empty() {
            return;
        }
        let mut token = Some(startup_id).filter(|t| !t.is_empty());
        let tx = self.tx.lock().unwrap();
        for show in shows {
            let _ = tx.send(NewWindow { show, token: token.take() });
        }
        if let Some(w) = self.wake.get() {
            w();
        }
    }
}

/// <https://www.freedesktop.org/wiki/Specifications/file-manager-interface/>
struct FileManager(Inbox);

fn paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter().filter_map(|u| path_of_uri(u)).collect()
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, startup_id: String) {
        self.0.deliver(paths(&uris).into_iter().map(|folder| Show { folder, select: Vec::new() }).collect(), startup_id);
    }

    fn show_items(&self, uris: Vec<String>, startup_id: String) {
        self.0.deliver(Show::items(&paths(&uris)), startup_id);
    }

    /// No properties window: show the items.
    fn show_item_properties(&self, uris: Vec<String>, startup_id: String) {
        self.0.deliver(Show::items(&paths(&uris)), startup_id);
    }
}

/// Queue for the file manager name, to take "show in folder" requests.
/// `address`: a bus address (tests), else the session bus. `first_here`:
/// this Fractale was started by the bus for a request (see `Remote`).
pub fn serve(address: Option<&str>, first_here: bool) -> Result<Remote, String> {
    let builder = match address {
        Some(a) => Builder::address(a),
        None => Builder::session(),
    }
    .map_err(|e| e.to_string())?;
    let conn: Connection = builder.build().map_err(|e| e.to_string())?;
    let (tx, rx) = channel();
    let inbox = Inbox { tx: Arc::new(Mutex::new(tx)), wake: Arc::new(OnceLock::new()) };
    // The object first, so a request arriving right after the name is
    // taken finds it.
    conn.object_server().at(FILE_MANAGER_PATH, FileManager(inbox.clone())).map_err(|e| e.to_string())?;
    // Queue (no replacing whoever has it). Not getting it is fine.
    let _ = conn.request_name_with_flags(FILE_MANAGER, Default::default());
    Ok(Remote { rx, first_here, wake: inbox.wake, _keep: Box::new(conn) })
}
