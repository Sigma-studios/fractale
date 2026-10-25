//! One running Fractale, and the desktop's file manager interface, over the
//! session bus.
//!
//! The first Fractale owns `io.github.Sigma_studios.Fractale`; a second
//! launch finds the name taken, hands its folder (and selection) to the
//! first and exits. The first also queues for
//! `org.freedesktop.FileManager1`, so browsers' and other apps' "show in
//! folder" open in it: it gets the name when no other file manager holds
//! it, or when the one holding it quits (taking it from Dolphin would be
//! rude; which file manager is the default is the desktop's choice).

use super::remote::{path_of_uri, Remote, Show, Wake};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

pub const NAME: &str = "io.github.Sigma_studios.Fractale";
const PATH: &str = "/io/github/Sigma_studios/Fractale";
const IFACE: &str = "io.github.Sigma_studios.Fractale";
const FILE_MANAGER: &str = "org.freedesktop.FileManager1";
const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";

pub enum Claim {
    /// This is the running Fractale; requests arrive here.
    Primary(Remote),
    /// Another Fractale took the request.
    Forwarded,
}

#[derive(Clone)]
struct Inbox {
    tx: Arc<Mutex<Sender<Show>>>,
    wake: Arc<OnceLock<Wake>>,
}

impl Inbox {
    fn deliver(&self, show: Option<Show>) {
        let Some(show) = show else { return };
        let _ = self.tx.lock().unwrap().send(show);
        if let Some(w) = self.wake.get() {
            w();
        }
    }
}

/// Fractale's own interface: `Show(folder, names)`, paths as bytes.
struct App(Inbox);

#[zbus::interface(name = "io.github.Sigma_studios.Fractale")]
impl App {
    fn show(&self, folder: Vec<u8>, select: Vec<Vec<u8>>) {
        let folder = PathBuf::from(OsString::from_vec(folder));
        self.0.deliver(Some(Show { folder, select: select.into_iter().map(OsString::from_vec).collect() }));
    }
}

/// <https://www.freedesktop.org/wiki/Specifications/file-manager-interface/>
struct FileManager(Inbox);

fn paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter().filter_map(|u| path_of_uri(u)).collect()
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, _startup_id: String) {
        // One window: the first folder.
        self.0.deliver(paths(&uris).into_iter().next().map(|folder| Show { folder, select: Vec::new() }));
    }

    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        self.0.deliver(Show::items(&paths(&uris)));
    }

    /// No properties window: show the items.
    fn show_item_properties(&self, uris: Vec<String>, _startup_id: String) {
        self.0.deliver(Show::items(&paths(&uris)));
    }
}

/// Become the running Fractale, or hand `first` to the one running.
/// `address`: a bus address (tests), else the session bus.
pub fn claim(address: Option<&str>, first: &Show) -> Result<Claim, String> {
    let builder = match address {
        Some(a) => Builder::address(a),
        None => Builder::session(),
    }
    .map_err(|e| e.to_string())?;
    let conn: Connection = builder.build().map_err(|e| e.to_string())?;
    let (tx, rx) = channel();
    let inbox = Inbox { tx: Arc::new(Mutex::new(tx)), wake: Arc::new(OnceLock::new()) };
    // Objects first, so a request arriving right after the name is taken
    // finds them.
    let server = conn.object_server();
    server.at(PATH, App(inbox.clone())).map_err(|e| e.to_string())?;
    server.at(FILE_MANAGER_PATH, FileManager(inbox.clone())).map_err(|e| e.to_string())?;
    drop(server);
    // zbus reports "Exists" as an error.
    let taken = match conn.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => true,
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => false,
        Err(e) => return Err(e.to_string()),
    };
    if taken {
        let select: Vec<Vec<u8>> = first.select.iter().map(|n| n.as_bytes().to_vec()).collect();
        conn.call_method(Some(NAME), PATH, Some(IFACE), "Show", &(first.folder.as_os_str().as_bytes(), select)).map_err(|e| e.to_string())?;
        return Ok(Claim::Forwarded);
    }
    // Queue for the file manager name (no replacing whoever has it).
    let _ = conn.request_name_with_flags(FILE_MANAGER, Default::default());
    Ok(Claim::Primary(Remote { rx, wake: inbox.wake, _keep: Box::new(conn) }))
}
