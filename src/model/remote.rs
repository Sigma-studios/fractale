//! Requests from outside the window: a second launch, or another app's
//! "show in folder" (see `dbus`). One window serves them all: it goes to
//! the folder, selects the items and asks to be raised.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, PartialEq)]
pub struct Show {
    pub folder: PathBuf,
    /// Names in `folder` to select.
    pub select: Vec<OsString>,
}

impl Show {
    /// Show `paths`: the first one's folder, with those of them that are in
    /// it selected.
    pub fn items(paths: &[PathBuf]) -> Option<Show> {
        let first = paths.first()?;
        let folder = first.parent().unwrap_or(Path::new("/")).to_path_buf();
        let select = paths.iter().filter(|p| p.parent() == Some(&folder)).filter_map(|p| p.file_name()).map(|n| n.to_owned()).collect();
        Some(Show { folder, select })
    }
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The receiving end, held by the window.
pub struct Remote {
    pub rx: Receiver<Show>,
    /// Set once the window exists, so requests repaint it.
    pub wake: Arc<OnceLock<Wake>>,
    /// Keeps the bus connection (and its names) alive.
    pub _keep: Box<dyn std::any::Any>,
}

/// `file:///a%20b` → `/a b` (bytes, so any name survives).
pub fn path_of_uri(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // A host part ("file://localhost/…") is allowed and ignored.
    let rest = &rest[rest.find('/')?..];
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_items() {
        assert_eq!(path_of_uri("file:///a%20b/%C3%A9.txt"), Some(PathBuf::from("/a b/é.txt")));
        assert_eq!(path_of_uri("file://localhost/x"), Some(PathBuf::from("/x")));
        assert_eq!(path_of_uri("https://x/y"), None);
        assert_eq!(path_of_uri("file:///bad%2"), None);
        let s = Show::items(&["/d/a".into(), "/d/b".into(), "/e/c".into()]).unwrap();
        assert_eq!(s, Show { folder: "/d".into(), select: vec!["a".into(), "b".into()] });
    }
}
