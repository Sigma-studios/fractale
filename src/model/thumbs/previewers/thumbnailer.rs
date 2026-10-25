//! The desktop's installed thumbnailers (`*.thumbnailer` files under
//! `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, the convention GNOME and others
//! follow): videos with ffmpegthumbnailer or totem, PDFs with evince or
//! poppler, fonts, office documents… Whatever the system has, Fractale
//! uses, so `nix-env -iA ffmpegthumbnailer` is how to add video previews.
//!
//! A file's MIME type comes from shared-mime-info (by name, then content;
//! see `apps`), so a video without an extension still finds its
//! thumbnailer. Thumbnailers run as a child process of the low-priority
//! worker (so they inherit its priority), with a timeout, writing to a
//! private temporary file.

use super::super::{FileInfo, Previewer, Rendered};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a thumbnailer may take (videos on a slow disk can take a few
/// seconds; a hung one is killed).
const TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(10) };

/// One `.thumbnailer` file.
pub struct Thumbnailer {
    name: &'static str,
    exec: Vec<String>,
    mime: Vec<String>,
    db: Arc<MimeDb>,
}

use crate::model::apps::MimeDb;

/// Every installed thumbnailer whose program exists, user's first (they
/// override the system's).
pub fn installed() -> Vec<Box<dyn Previewer>> {
    installed_in(&data_dirs())
}

pub fn installed_in(dirs: &[PathBuf]) -> Vec<Box<dyn Previewer>> {
    let db = Arc::new(MimeDb::load(dirs));
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Box<dyn Previewer>> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir.join("thumbnailers")) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "thumbnailer")).collect();
        files.sort();
        for f in files {
            // The same file name in a later directory is shadowed.
            if !seen.insert(f.file_name().map(|n| n.to_owned())) {
                continue;
            }
            if let Some(t) = parse(&f, db.clone()) {
                out.push(Box::new(t));
            }
        }
    }
    out
}

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|s| s.starts_with('/')).map(PathBuf::from));
    dirs
}

fn parse(path: &Path, db: Arc<MimeDb>) -> Option<Thumbnailer> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let (mut exec, mut try_exec, mut mime) = (None, None, Vec::new());
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Thumbnailer Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if !in_entry {
            continue;
        }
        match k.trim() {
            "Exec" => exec = Some(v.trim().to_string()),
            "TryExec" => try_exec = Some(v.trim().to_string()),
            "MimeType" => mime = v.split(';').map(str::trim).filter(|m| !m.is_empty()).map(String::from).collect(),
            _ => {}
        }
    }
    let exec = split_exec(&exec?);
    let program = try_exec.unwrap_or_else(|| exec.first().cloned().unwrap_or_default());
    if mime.is_empty() || find_program(&program).is_none() {
        return None;
    }
    let name: &'static str = Box::leak(path.file_stem()?.to_string_lossy().into_owned().into_boxed_str());
    Some(Thumbnailer { name, exec, mime, db })
}

/// The Exec line split like a shell would for the simple quoting desktop
/// files use.
fn split_exec(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => cur.extend(chars.next()),
            ' ' | '\t' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn find_program(p: &str) -> Option<PathBuf> {
    if p.contains('/') {
        return Path::new(p).is_file().then(|| p.into());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(p)).find(|c| c.is_file())
}

impl Previewer for Thumbnailer {
    fn name(&self) -> &'static str {
        self.name
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        // Found once per file (every thumbnailer asks), by name then content.
        let m = f.mime.get_or_init(|| self.db.type_of(&f.path));
        self.mime.iter().any(|t| t == m || self.db.is_a(m, t) || (t.ends_with("/*") && m.starts_with(&t[..t.len() - 1])))
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        let tmp = TempDir::new()?;
        let out = tmp.0.join("thumb.png");
        let uri = super::super::cache::uri(&f.path);
        let mut args = self.exec.iter().map(|a| {
            let mut s = String::new();
            let mut it = a.chars();
            while let Some(c) = it.next() {
                if c != '%' {
                    s.push(c);
                    continue;
                }
                match it.next() {
                    Some('i') => s.push_str(&f.path.to_string_lossy()),
                    Some('u') => s.push_str(&uri),
                    Some('o') => s.push_str(&out.to_string_lossy()),
                    Some('s') => s.push_str(&max.to_string()),
                    Some('%') => s.push('%'),
                    _ => {}
                }
            }
            s
        });
        let program = args.next().ok_or("empty Exec")?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Its own process group, so a timeout kills what it started too.
            .process_group(0)
            .spawn()
            .map_err(|e| e.to_string())?;
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            if start.elapsed() > TIMEOUT {
                // SAFETY: plain syscall; the group is the child's own (it
                // hasn't been reaped, so its id can't have been reused).
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
                return Err(format!("{} timed out", self.name));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(format!("{} failed: {}", self.name, err.trim()));
        }
        let img = image::open(&out).map_err(|e| e.to_string())?.into_rgba8();
        Ok(Rendered::Still(super::shrink(img, max)))
    }
}

/// A private temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("fractale-thumb-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| e.to_string())?;
        Ok(TempDir(dir))
    }
}

use std::os::unix::fs::DirBuilderExt;

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn exec_lines_split() {
        assert_eq!(split_exec(r#"ffmpegthumbnailer -i %i -o %o -s %s -f"#), ["ffmpegthumbnailer", "-i", "%i", "-o", "%o", "-s", "%s", "-f"]);
        assert_eq!(split_exec(r#"/usr/bin/x "a b" "" c"#), ["/usr/bin/x", "a b", "", "c"]);
    }

    /// A fake thumbnailer in a fake data directory: a script that writes a
    /// PNG of the requested size, another that hangs.
    #[test]
    fn runs_installed_thumbnailers() {
        let dir = std::env::temp_dir().join(format!("fractale-thumbnailers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("thumbnailers")).unwrap();
        std::fs::create_dir_all(dir.join("mime")).unwrap();
        std::fs::write(dir.join("mime/globs2"), "50:video/x-fake:*.fake\n50:video/x-slow:*.slow\n50:application/x-gz:*.gz\n55:application/x-tgz:*.tar.gz\n").unwrap();
        let png = dir.join("red.png");
        image::RgbaImage::from_pixel(300, 200, image::Rgba([255, 0, 0, 255])).save(&png).unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let ok = script("fake.sh", &format!("[ \"$3\" = 256 ] || exit 3; cp {} \"$2\"", png.display()));
        let slow = script("slow.sh", "sleep 30");
        std::fs::write(dir.join("thumbnailers/fake.thumbnailer"), format!("[Thumbnailer Entry]\nTryExec={0}\nExec={0} %i %o %s\nMimeType=video/x-fake;\n", ok.display())).unwrap();
        std::fs::write(dir.join("thumbnailers/slow.thumbnailer"), format!("[Thumbnailer Entry]\nExec={} %i %o\nMimeType=video/*;\n", slow.display())).unwrap();
        std::fs::write(dir.join("thumbnailers/missing.thumbnailer"), "[Thumbnailer Entry]\nExec=/nonexistent/x %i %o\nMimeType=video/x-fake;\n").unwrap();

        let all = installed_in(std::slice::from_ref(&dir));
        assert_eq!(all.iter().map(|t| t.name()).collect::<Vec<_>>(), ["fake", "slow"]);
        let db = MimeDb::load(std::slice::from_ref(&dir));
        assert_eq!(db.type_of_name("a.tar.gz").as_deref(), Some("application/x-tgz"));
        assert_eq!(db.type_of_name("A.GZ").as_deref(), Some("application/x-gz"));

        let clip = FileInfo::new(dir.join("clip.fake"), 10, 0);
        let t = all.iter().find(|t| t.accepts(&clip)).unwrap();
        let img = t.render(&clip, 256).unwrap();
        assert_eq!(img.still().dimensions(), (256, 171));
        assert!(t.render(&clip, 128).is_err(), "a failing thumbnailer is an error");
        let slow_file = FileInfo::new(dir.join("x.slow"), 10, 0);
        let s = all.iter().find(|t| t.accepts(&slow_file)).unwrap();
        assert_eq!(s.name(), "slow", "video/* matches");
        let start = Instant::now();
        assert!(s.render(&slow_file, 128).is_err());
        assert!(start.elapsed() < TIMEOUT + Duration::from_secs(2));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
