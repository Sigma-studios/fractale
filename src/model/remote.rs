//! Requests from outside the window: another app's "show in folder" (see
//! `dbus`), and new windows. Every window is its own Fractale process; a
//! request opens a new window by starting another one.

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
    /// Show `paths`: one `Show` per folder they're in (in order of first
    /// appearance), each with its items selected.
    pub fn items(paths: &[PathBuf]) -> Vec<Show> {
        let mut out: Vec<Show> = Vec::new();
        for p in paths {
            let folder = p.parent().unwrap_or(Path::new("/")).to_path_buf();
            let Some(name) = p.file_name() else { continue };
            match out.iter_mut().find(|s| s.folder == folder) {
                Some(s) => s.select.push(name.to_owned()),
                None => out.push(Show { folder, select: vec![name.to_owned()] }),
            }
        }
        out
    }

    /// The command line that opens this in a new Fractale.
    pub fn args(&self) -> Vec<OsString> {
        if self.select.is_empty() {
            return vec![self.folder.clone().into_os_string()];
        }
        self.select.iter().flat_map(|n| [OsString::from("--select"), self.folder.join(n).into_os_string()]).collect()
    }
}

/// A window to open, with the activation token that lets it take the focus
/// (from the app that asked, or asked for by this window).
#[derive(Clone, Debug, PartialEq)]
pub struct NewWindow {
    pub show: Show,
    pub token: Option<String>,
}

/// Start another Fractale showing `show`. It runs on its own: its own
/// process group (a Ctrl+C in the terminal that started the first one
/// doesn't close it) and a thread that reaps it.
pub fn open_window(show: &Show, token: Option<&str>) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok().filter(|e| e.exists()).unwrap_or_else(|| "fractale".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(show.args()).stdin(std::process::Stdio::null()).process_group(0);
    cmd.env_remove("XDG_ACTIVATION_TOKEN").env_remove("DESKTOP_STARTUP_ID");
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        cmd.env("XDG_ACTIVATION_TOKEN", t);
    }
    let mut child = cmd.spawn()?;
    std::thread::Builder::new().name("window reaper".into()).spawn(move || child.wait())?;
    Ok(())
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The receiving end, held by the window.
pub struct Remote {
    pub rx: Receiver<NewWindow>,
    /// Started by the bus for a request: show the first one in this
    /// window rather than a new one.
    pub first_here: bool,
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
        let s = Show::items(&["/d/a".into(), "/e/c".into(), "/d/b".into()]);
        assert_eq!(s, vec![Show { folder: "/d".into(), select: vec!["a".into(), "b".into()] }, Show { folder: "/e".into(), select: vec!["c".into()] }]);
        assert_eq!(s[0].args(), ["--select", "/d/a", "--select", "/d/b"].map(OsString::from));
        assert_eq!(Show { folder: "/x y".into(), select: Vec::new() }.args(), [OsString::from("/x y")]);
    }
}
