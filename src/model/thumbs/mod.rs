//! Previews for the grid.
//!
//! Previewers are small, independent pieces behind one trait, tried in
//! order: images decoded in-process, animated GIFs, GIMP's XCF, Pixelle
//! sprites, Orchestre songs, Prisme documents, audio waveforms, and on
//! Linux every installed freedesktop `.thumbnailer` (videos, PDFs, …).
//! Adding a format is adding a previewer.
//!
//! A small pool of lower-priority workers serves only what the grid wants
//! right now (the visible tiles and a screen ahead); tiles that scrolled
//! away are dropped from the queue. Results go to the shared thumbnail
//! cache (see `cache`) so other apps benefit too.

pub mod cache;
pub mod previewers;

use cache::Bucket;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// What a previewer gets to look at.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub path: PathBuf,
    /// Lowercase extension, without the dot.
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    /// The MIME type, found by whichever previewer needs it first.
    pub mime: std::cell::OnceCell<String>,
}

impl FileInfo {
    pub fn new(path: PathBuf, size: u64, mtime: i64) -> FileInfo {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        FileInfo { path, ext, size, mtime, mime: Default::default() }
    }
}

/// A rendered preview.
#[derive(Clone, Debug)]
pub enum Rendered {
    Still(image::RgbaImage),
    /// Frames with their durations; the first is the still.
    Animated(Vec<(image::RgbaImage, Duration)>),
}

impl Rendered {
    pub fn still(&self) -> &image::RgbaImage {
        match self {
            Rendered::Still(i) => i,
            Rendered::Animated(f) => &f[0].0,
        }
    }
}

pub trait Previewer: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whether this previewer handles the file (from its name; content is
    /// checked in `render`).
    fn accepts(&self, f: &FileInfo) -> bool;
    /// An image whose larger side is at most `max` pixels (never upscaled
    /// beyond the source, except pixel art).
    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String>;
    /// Animated previews are rendered every time (only their first frame is
    /// cached); stills are cached and read back.
    fn animated(&self) -> bool {
        false
    }
}

/// Fit `w`×`h` into `max`×`max`, keeping the aspect, never growing.
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w.max(1), h.max(1));
    }
    let k = max as f64 / w.max(h) as f64;
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

/// One tile's preview, identified by file and size bucket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub path: PathBuf,
    pub mtime: i64,
    pub bucket: Bucket,
}

pub struct Request {
    pub key: Key,
    pub size: u64,
}

pub enum Ready {
    Done(Key, Rendered),
    /// No preview possible: show the type icon.
    Failed(Key),
}

struct Queue {
    /// Most wanted first.
    wanted: Vec<Request>,
    running: HashSet<Key>,
}

pub struct Thumbs {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    pub rx: Receiver<Ready>,
    stop: Arc<AtomicBool>,
}

impl Thumbs {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> Thumbs {
        Thumbs::spawn_with(cache::root(), previewers::registry, wake)
    }

    /// `previewers` runs on a worker (finding installed thumbnailers reads
    /// the disk).
    pub fn spawn_with(root: PathBuf, previewers: fn() -> Vec<Box<dyn Previewer>>, wake: Arc<dyn Fn() + Send + Sync>) -> Thumbs {
        let queue = Arc::new((Mutex::new(Queue { wanted: Vec::new(), running: HashSet::new() }), Condvar::new()));
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let n = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).clamp(2, 8));
        let (q, s) = (queue.clone(), stop.clone());
        std::thread::Builder::new()
            .name("thumbs-init".into())
            .spawn(move || {
                let previewers: Arc<Vec<Box<dyn Previewer>>> = Arc::new(previewers());
                for i in 0..n {
                    let (queue, tx, stop, previewers, wake, root) = (q.clone(), tx.clone(), s.clone(), previewers.clone(), wake.clone(), root.clone());
                    std::thread::Builder::new()
                        .name(format!("thumbs-{i}"))
                        .spawn(move || {
                            super::platform::lower_priority_current_thread();
                            worker(&queue, &tx, &stop, &previewers, &*wake, &root)
                        })
                        .expect("spawn thumbnail worker");
                }
            })
            .expect("spawn thumbnail workers");
        Thumbs { queue, rx, stop }
    }

    /// Replace what's wanted (most wanted first). Anything not in the list
    /// any more is dropped; what's running finishes.
    pub fn want(&self, wanted: Vec<Request>) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.wanted = wanted.into_iter().filter(|r| !q.running.contains(&r.key)).collect();
        cv.notify_all();
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.queue.1.notify_all();
    }
}

fn worker(queue: &(Mutex<Queue>, Condvar), tx: &Sender<Ready>, stop: &AtomicBool, previewers: &[Box<dyn Previewer>], wake: &(dyn Fn() + Send + Sync), root: &Path) {
    loop {
        let req = {
            let (lock, cv) = queue;
            let mut q = lock.lock().unwrap();
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if !q.wanted.is_empty() {
                    let r = q.wanted.remove(0);
                    q.running.insert(r.key.clone());
                    break r;
                }
                q = cv.wait(q).unwrap();
            }
        };
        let ready = make(&req, previewers, root);
        queue.0.lock().unwrap().running.remove(&req.key);
        if tx.send(ready).is_err() {
            return;
        }
        wake();
    }
}

/// Cache, then previewers in order, then the failure record.
pub fn make(req: &Request, previewers: &[Box<dyn Previewer>], root: &Path) -> Ready {
    let key = req.key.clone();
    if cache::is_inside(root, &key.path) {
        return Ready::Failed(key);
    }
    let info = FileInfo::new(key.path.clone(), req.size, key.mtime);
    let mut accepting = previewers.iter().filter(|p| p.accepts(&info)).peekable();
    let Some(first) = accepting.peek() else { return Ready::Failed(key) };
    if !first.animated() {
        if let Some(img) = cache::read(root, key.bucket, &key.path, key.mtime) {
            return Ready::Done(key, Rendered::Still(img));
        }
    }
    if cache::failed(root, &key.path, key.mtime) {
        return Ready::Failed(key);
    }
    // The first that succeeds (a system thumbnailer may know a format
    // variant the built-in decoder doesn't).
    for p in accepting {
        if let Ok(r) = p.render(&info, key.bucket.pixels()) {
            let _ = cache::write(root, key.bucket, &key.path, key.mtime, req.size, r.still());
            return Ready::Done(key, r);
        }
    }
    let _ = cache::write_failure(root, &key.path, key.mtime);
    Ready::Failed(key)
}
