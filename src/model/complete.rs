//! Path completion for the location field, on a worker thread: the
//! subfolders of the typed parent that start with the typed name.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Completer {
    tx: Sender<String>,
    rx: Receiver<(String, Vec<String>)>,
    asked: String,
}

impl Completer {
    pub fn spawn() -> Completer {
        let (tx, jobs) = channel::<String>();
        let (out, rx) = channel();
        std::thread::Builder::new()
            .name("complete".into())
            .spawn(move || {
                while let Ok(mut text) = jobs.recv() {
                    // Only the latest question matters.
                    while let Ok(newer) = jobs.try_recv() {
                        text = newer;
                    }
                    let found = candidates(&text);
                    if out.send((text, found)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn completer");
        Completer { tx, rx, asked: String::new() }
    }

    pub fn ask(&mut self, text: String) {
        if text != self.asked {
            self.asked = text.clone();
            let _ = self.tx.send(text);
        }
    }

    /// The newest answer for the text last asked about, if any arrived.
    pub fn poll(&mut self) -> Option<Vec<String>> {
        let mut latest = None;
        while let Ok((text, found)) = self.rx.try_recv() {
            if text == self.asked {
                latest = Some(found);
            }
        }
        latest
    }
}

/// Subfolders matching what's typed after the last `/`, as full paths
/// with a trailing `/`. Hidden ones only when the typed name starts with a
/// dot. Case-sensitive matches first, then case-insensitive ones.
pub fn candidates(text: &str) -> Vec<String> {
    let text = crate::ui::topbar::expand_tilde(text);
    let Some(slash) = text.rfind('/') else { return Vec::new() };
    let (parent, prefix) = (&text[..=slash], &text[slash + 1..]);
    let Ok(rd) = std::fs::read_dir(Path::new(parent)) else { return Vec::new() };
    let lower = prefix.to_lowercase();
    let mut exact = Vec::new();
    let mut loose = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !prefix.starts_with('.') {
            continue;
        }
        let is_dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
        if !is_dir {
            continue;
        }
        let full = format!("{parent}{name}/");
        if name.starts_with(prefix) {
            exact.push(full);
        } else if name.to_lowercase().starts_with(&lower) {
            loose.push(full);
        }
    }
    exact.sort();
    loose.sort();
    exact.extend(loose);
    exact
}

#[cfg(test)]
mod tests {
    #[test]
    fn completes_subfolders() {
        let d = std::env::temp_dir().join(format!("fractale-complete-{}", std::process::id()));
        for n in ["Projects", "Pictures", "public", ".private", "file"] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        std::fs::remove_dir(d.join("file")).unwrap();
        std::fs::write(d.join("file"), b"x").unwrap();
        let base = format!("{}/", d.display());
        let got = super::candidates(&format!("{base}P"));
        assert_eq!(got, [format!("{base}Pictures/"), format!("{base}Projects/"), format!("{base}public/")]);
        assert_eq!(super::candidates(&format!("{base}.p")), [format!("{base}.private/")]);
        assert!(super::candidates(&format!("{base}fi")).is_empty(), "files aren't completed");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
