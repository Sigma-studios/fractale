//! Listing the open folder: `read_dir` + `lstat` on a worker thread, kept
//! current by a non-recursive `notify` watch.
//!
//! A folder being written into (a build, a download) can change thousands of
//! times a second. The first change is listed after a short settle; after
//! that, at most every [`REFRESH_EVERY`], so the view never thrashes.
//! Subfolders' item counts are computed after each listing, only for the
//! ones whose mtime changed since they were last counted.

use super::entry::{Entry, Kind};
use notify::{RecursiveMode, Watcher};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const REFRESH_EVERY: Duration = Duration::from_millis(250);
/// Wait this long after the first change before listing, to catch the rest
/// of a burst (a save often is several events).
const SETTLE: Duration = Duration::from_millis(30);

#[derive(Debug, Default)]
pub struct Listing {
    pub path: PathBuf,
    /// Navigation generation this listing belongs to.
    pub generation: u64,
    /// Incremented by every refresh of the same folder.
    pub refresh: u64,
    pub entries: Vec<Entry>,
    /// The folder itself couldn't be read.
    pub error: Option<String>,
    /// Device of the folder (to spot mount points among subfolders).
    pub dev: u64,
    /// The filesystem's (total, used) bytes, as `df` shows them.
    pub space: Option<(u64, u64)>,
    pub took: Duration,
}

/// Entries in a subfolder, or why there's no count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    Items(u64),
    Unreadable,
}

pub enum Msg {
    Listed(Arc<Listing>),
    Counts { generation: u64, counts: Vec<(OsString, Count)> },
}

enum Cmd {
    Open { path: PathBuf, generation: u64 },
    Refresh,
    Changed { generation: u64 },
}

pub struct Lister {
    tx: Sender<Cmd>,
    pub rx: Receiver<Msg>,
}

impl Lister {
    /// `wake` is called (from the worker) whenever a message is waiting.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Lister {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (msg_tx, rx) = channel::<Msg>();
        let watch_tx = tx.clone();
        let wake = Arc::new(wake);
        std::thread::Builder::new()
            .name("lister".into())
            .spawn(move || run(cmd_rx, watch_tx, msg_tx, wake))
            .expect("spawn lister");
        Lister { tx, rx }
    }

    pub fn open(&self, path: PathBuf, generation: u64) {
        let _ = self.tx.send(Cmd::Open { path, generation });
    }

    /// List again now (F5), without waiting for the watcher.
    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }
}

struct State {
    path: PathBuf,
    generation: u64,
    refresh: u64,
    watcher: Option<notify::RecommendedWatcher>,
    /// When the first unlisted change arrived.
    dirty_since: Option<Instant>,
    last_listed: Instant,
    /// Subfolder counts already sent. Shared with the counting task;
    /// cleared on navigation.
    counted: Counted,
}

/// name -> (mtime, ino, count) of subfolders already counted.
type Counted = Arc<Mutex<HashMap<OsString, (i64, u64, Count)>>>;

fn run(rx: Receiver<Cmd>, watch_tx: Sender<Cmd>, out: Sender<Msg>, wake: Arc<dyn Fn() + Send + Sync>) {
    let mut st = State {
        path: PathBuf::new(),
        generation: 0,
        refresh: 0,
        watcher: None,
        dirty_since: None,
        last_listed: Instant::now(),
        counted: Arc::new(Mutex::new(HashMap::new())),
    };
    loop {
        let timeout = match st.dirty_since {
            Some(first) => {
                let due = (first + SETTLE).max(st.last_listed + REFRESH_EVERY);
                due.saturating_duration_since(Instant::now())
            }
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(timeout) {
            Ok(Cmd::Open { path, generation }) => {
                st.path = path;
                st.generation = generation;
                st.refresh = 0;
                st.counted = Arc::new(Mutex::new(HashMap::new()));
                st.dirty_since = None;
                st.watcher = watch(&st.path, generation, watch_tx.clone());
                list_and_send(&mut st, &out, &wake);
            }
            Ok(Cmd::Refresh) => {
                st.dirty_since = None;
                list_and_send(&mut st, &out, &wake);
            }
            Ok(Cmd::Changed { generation }) => {
                if generation == st.generation && st.dirty_since.is_none() {
                    st.dirty_since = Some(Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if st.dirty_since.take().is_some() {
                    list_and_send(&mut st, &out, &wake);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn watch(path: &Path, generation: u64, tx: Sender<Cmd>) -> Option<notify::RecommendedWatcher> {
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // Access events (reads) don't change what we show.
        if res.as_ref().is_ok_and(|e| matches!(e.kind, notify::EventKind::Access(_))) {
            return;
        }
        let _ = tx.send(Cmd::Changed { generation });
    })
    .ok()?;
    w.watch(path, RecursiveMode::NonRecursive).ok()?;
    Some(w)
}

fn list_and_send(st: &mut State, out: &Sender<Msg>, wake: &Arc<dyn Fn() + Send + Sync>) {
    let listing = list(&st.path, st.generation, st.refresh);
    st.refresh += 1;
    st.last_listed = Instant::now();
    let generation = listing.generation;
    // Count only subfolders that are new or changed (a folder's mtime moves
    // when entries are added or removed).
    let todo: Vec<(OsString, i64, u64)> = {
        let counted = st.counted.lock().unwrap();
        listing
            .entries
            .iter()
            .filter(|e| e.kind == Kind::Dir)
            .filter(|e| !counted.get(&e.name).is_some_and(|c| c.0 == e.mtime && c.1 == e.ino))
            .map(|e| (e.name.clone(), e.mtime, e.ino))
            .collect()
    };
    let _ = out.send(Msg::Listed(Arc::new(listing)));
    wake();
    if todo.is_empty() {
        return;
    }
    // Off this thread, so a navigation is never kept waiting by counting.
    let (path, counted, out, wake) = (st.path.clone(), st.counted.clone(), out.clone(), wake.clone());
    rayon::spawn(move || {
        use rayon::prelude::*;
        let counts: Vec<(OsString, i64, u64, Count)> = todo
            .into_par_iter()
            .map(|(name, mtime, ino)| {
                let count = match std::fs::read_dir(path.join(&name)) {
                    Ok(rd) => Count::Items(rd.count() as u64),
                    Err(_) => Count::Unreadable,
                };
                (name, mtime, ino, count)
            })
            .collect();
        let mut c = counted.lock().unwrap();
        for (name, mtime, ino, count) in &counts {
            c.insert(name.clone(), (*mtime, *ino, *count));
        }
        drop(c);
        let _ = out.send(Msg::Counts { generation, counts: counts.into_iter().map(|(n, _, _, c)| (n, c)).collect() });
        wake();
    });
}

/// `read_dir` and `lstat` of every entry. Entries that vanish meanwhile are
/// skipped.
pub fn list(path: &Path, generation: u64, refresh: u64) -> Listing {
    let start = Instant::now();
    let mut listing = Listing { path: path.to_path_buf(), generation, refresh, ..Default::default() };
    use std::os::unix::fs::MetadataExt;
    listing.dev = std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0);
    listing.space = space(path);
    match std::fs::read_dir(path) {
        Ok(rd) => {
            for e in rd.flatten() {
                let name = e.file_name();
                if let Ok(md) = std::fs::symlink_metadata(e.path()) {
                    listing.entries.push(Entry::from_metadata(name, &md, path));
                }
            }
        }
        Err(e) => listing.error = Some(describe(&e)),
    }
    listing.took = start.elapsed();
    listing
}

pub fn describe(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "You don't have permission to open this folder".into(),
        std::io::ErrorKind::NotFound => "This folder doesn't exist (any more)".into(),
        _ => e.to_string(),
    }
}

/// (total, used) bytes of the filesystem holding `path`.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter; `c` is a valid C string.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return None;
        }
        let bs = s.f_frsize as u64;
        #[allow(clippy::unnecessary_cast)]
        let (blocks, free) = (s.f_blocks as u64, s.f_bfree as u64);
        Some((blocks * bs, blocks.saturating_sub(free) * bs))
    }
}
