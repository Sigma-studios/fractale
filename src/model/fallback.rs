//! Sizing folders without répertoire, with répertoire's own scanner
//! (`repertoire-core`), so the numbers don't jump when the daemon starts.
//!
//! A worker thread sizes the open folder's subfolders one by one, visible
//! ones first, on a small pool at idle priority. Each finished subfolder is
//! grafted into one tree for the folder, which is what charges a hard link
//! once even when its links are in two subfolders. Results stream out as
//! they come. Navigating away cancels the walk in progress.
//!
//! Trees are kept for the session (a few of them), so opening a subfolder
//! that was just sized, or expanding one, answers at once from the tree.

use super::platform;
use super::sizes::{ChildSize, FolderSizes, SizeMsg, SourceState};
use repertoire_core::{Scanner, Tree, ROOT};
use std::collections::{HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Trees kept for the session.
const KEEP_TREES: usize = 4;
/// Entries a kept tree may have in total (about 100 bytes each).
const KEEP_ENTRIES: usize = 3_000_000;
/// Re-measure a cached folder when it's this old.
const MAX_AGE: Duration = Duration::from_secs(10 * 60);

enum Cmd {
    /// Size `folder`'s subfolders (`order`: names, most wanted first) and
    /// the contents of each `expanded` folder.
    Watch { folder: PathBuf, order: Vec<OsString>, expanded: Vec<PathBuf> },
    /// Some subfolders of `folder` changed: size them again.
    Changed { folder: PathBuf, names: Vec<OsString> },
    /// Forget everything about `folder` and below (F5).
    Forget { folder: PathBuf },
    /// Stop working (répertoire took over).
    Idle,
}

pub struct Fallback {
    tx: Sender<Cmd>,
    pub rx: Receiver<SizeMsg>,
    /// The cancel flag of the walk in progress.
    cancel: Arc<Mutex<Arc<AtomicBool>>>,
    watching: PathBuf,
}

impl Fallback {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> Fallback {
        let (tx, cmds) = channel();
        let (out, rx) = channel();
        let cancel = Arc::new(Mutex::new(Arc::new(AtomicBool::new(false))));
        let c = cancel.clone();
        std::thread::Builder::new()
            .name("sizer".into())
            .spawn(move || Worker::new(out, wake, c).run(cmds))
            .expect("spawn sizer");
        Fallback { tx, rx, cancel, watching: PathBuf::new() }
    }

    pub fn watch(&mut self, folder: &Path, order: Vec<OsString>, expanded: Vec<PathBuf>) {
        if folder != self.watching {
            // Stop the walk now rather than when the worker gets to it.
            self.cancel.lock().unwrap().store(true, Ordering::Relaxed);
            self.watching = folder.to_path_buf();
        }
        let _ = self.tx.send(Cmd::Watch { folder: folder.to_path_buf(), order, expanded });
    }

    pub fn changed(&self, folder: &Path, names: Vec<OsString>) {
        let _ = self.tx.send(Cmd::Changed { folder: folder.to_path_buf(), names });
    }

    /// Stop measuring: another source answers now.
    pub fn idle(&mut self) {
        self.cancel.lock().unwrap().store(true, Ordering::Relaxed);
        self.watching = PathBuf::new();
        let _ = self.tx.send(Cmd::Idle);
    }

    pub fn forget(&self, folder: &Path) {
        let _ = self.tx.send(Cmd::Forget { folder: folder.to_path_buf() });
    }
}

/// A tree for one folder (its root) and how it was made.
struct Kept {
    root: PathBuf,
    tree: Tree,
    complete: bool,
    made: Instant,
}

/// Work for one folder: which subfolders are left to size.
struct Job {
    folder: PathBuf,
    /// Index into `kept` of the folder's tree.
    todo: VecDeque<OsString>,
    /// Subfolders that are other filesystems: sized last, separately.
    mounts: Vec<OsString>,
    cancel: Arc<AtomicBool>,
}

struct Worker {
    out: Sender<SizeMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    cancel_slot: Arc<Mutex<Arc<AtomicBool>>>,
    kept: Vec<Kept>,
    pool: rayon::ThreadPool,
    /// The folder shown and what's expanded in it.
    folder: PathBuf,
    expanded: Vec<PathBuf>,
    order: Vec<OsString>,
    jobs: VecDeque<Job>,
    /// Expanded folders waiting for their part of a tree to be sized.
    waiting: Vec<PathBuf>,
}

impl Worker {
    fn new(out: Sender<SizeMsg>, wake: Arc<dyn Fn() + Send + Sync>, cancel_slot: Arc<Mutex<Arc<AtomicBool>>>) -> Worker {
        let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 4));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("sizer-{i}"))
            .start_handler(|_| platform::deprioritize_current_thread())
            .build()
            .expect("sizer pool");
        Worker { out, wake, cancel_slot, kept: Vec::new(), pool, folder: PathBuf::new(), expanded: Vec::new(), order: Vec::new(), jobs: VecDeque::new(), waiting: Vec::new() }
    }

    fn send(&self, msg: SizeMsg) {
        let _ = self.out.send(msg);
        (self.wake)();
    }

    fn run(mut self, cmds: Receiver<Cmd>) {
        platform::deprioritize_current_thread();
        loop {
            // Block for commands only when there's nothing to do.
            let cmd = if self.jobs.is_empty() {
                self.send(SizeMsg::State(SourceState::OwnScan { busy: false }));
                match cmds.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            } else {
                match cmds.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            };
            if let Some(c) = cmd {
                self.handle(c);
                continue;
            }
            self.step();
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Watch { folder, order, expanded } => {
                let moved = folder != self.folder;
                self.folder = folder;
                self.order = order;
                let new_expanded: Vec<PathBuf> = expanded.iter().filter(|p| !self.expanded.contains(p)).cloned().collect();
                self.expanded = expanded;
                if moved {
                    self.jobs.clear();
                    self.waiting.clear();
                    let fresh = Arc::new(AtomicBool::new(false));
                    *self.cancel_slot.lock().unwrap() = fresh;
                    let f = self.folder.clone();
                    self.start(&f);
                    for p in self.expanded.clone() {
                        self.start(&p);
                    }
                } else {
                    for p in new_expanded {
                        self.start(&p);
                    }
                    self.reprioritize();
                }
            }
            Cmd::Changed { folder, names } => {
                // Ancestors' trees are out of date now; this folder's is
                // patched below.
                self.kept.retain(|k| !(folder.starts_with(&k.root) && folder != k.root));
                if let Some(k) = self.kept.iter_mut().find(|k| k.root == folder) {
                    k.complete = false;
                }
                if !self.kept.iter().any(|k| k.root == folder) {
                    self.start(&folder);
                    return;
                }
                self.refresh_files(&folder);
                if let Some(job) = self.jobs.iter_mut().find(|j| j.folder == folder) {
                    for n in names {
                        if !job.todo.contains(&n) {
                            job.todo.push_front(n);
                        }
                    }
                } else {
                    let cancel = self.cancel_slot.lock().unwrap().clone();
                    self.jobs.push_front(Job { folder, todo: names.into(), mounts: Vec::new(), cancel });
                }
            }
            Cmd::Idle => {
                self.jobs.clear();
                self.waiting.clear();
                self.folder = PathBuf::new();
                *self.cancel_slot.lock().unwrap() = Arc::new(AtomicBool::new(false));
            }
            Cmd::Forget { folder } => {
                self.kept.retain(|k| !k.root.starts_with(&folder) && !folder.starts_with(&k.root));
                if folder == self.folder {
                    self.jobs.clear();
                    self.start(&folder.clone());
                    for p in self.expanded.clone() {
                        self.start(&p);
                    }
                }
            }
        }
    }

    /// Visible subfolders of the open folder first.
    fn reprioritize(&mut self) {
        let order = self.order.clone();
        if let Some(job) = self.jobs.iter_mut().find(|j| j.folder == self.folder) {
            let rank = |n: &OsString| order.iter().position(|o| o == n).unwrap_or(usize::MAX);
            let mut v: Vec<OsString> = std::mem::take(&mut job.todo).into();
            v.sort_by_key(rank);
            job.todo = v.into();
        }
    }

    /// Answer from a kept tree if one covers `folder`, else queue a job.
    /// A folder inside the open one is answered from the open folder's tree
    /// once its subfolder there is sized (see `answer_waiting`).
    fn start(&mut self, folder: &Path) {
        if let Some(sizes) = self.sizes_in_kept(folder) {
            self.send(SizeMsg::Folder(sizes));
            return;
        }
        if self.jobs.iter().any(|j| j.folder == folder) {
            return;
        }
        if folder != self.folder && folder.starts_with(&self.folder) && self.jobs.iter().any(|j| j.folder == self.folder) {
            self.waiting.push(folder.to_path_buf());
            return;
        }
        let cancel = self.cancel_slot.lock().unwrap().clone();
        match self.begin(folder) {
            Some((todo, mounts)) => self.jobs.push_back(Job { folder: folder.to_path_buf(), todo, mounts, cancel }),
            None => self.send(SizeMsg::Done { folder: folder.to_path_buf(), total: 0, files_here: 0, hidden: 0, complete: true }),
        }
    }

    /// The sizes of `folder` from a recent kept tree: a complete one, or
    /// any tree where the subfolder containing `folder` is already sized.
    fn sizes_in_kept(&self, folder: &Path) -> Option<FolderSizes> {
        let k = self.kept.iter().find(|k| {
            if k.made.elapsed() >= MAX_AGE || !folder.starts_with(&k.root) {
                return false;
            }
            if folder == k.root {
                return k.complete;
            }
            // Its top-level subfolder in the tree has been sized.
            let first = folder.strip_prefix(&k.root).ok().and_then(|r| r.components().next());
            let m = k.tree.mounts()[0];
            k.complete || first.is_some_and(|c| k.tree.child(m, c.as_os_str().as_bytes()).is_some() && !self.is_pending(&k.root, c.as_os_str()))
        })?;
        let id = k.tree.lookup(folder)?;
        let tree = &k.tree;
        let d = tree.dir(id)?;
        let mut fs = FolderSizes::new(folder.to_path_buf());
        for &c in tree.children(id) {
            let n = tree.node(c);
            if let Some(cd) = tree.dir(c) {
                fs.children.insert(
                    n.name().to_owned(),
                    ChildSize { total: cd.total, files: cd.files, dirs: cd.dirs, mtime: n.mtime, unreadable: tree.is_unreadable(c), mount: None },
                );
            }
        }
        // Mounts below aren't in the tree; size them separately.
        for (name, path) in mounts_in(folder, d.dev) {
            if let Some(size) = self.size_mount(&path) {
                fs.children.insert(name, size);
            }
        }
        fs.files_here = d.own;
        fs.total = d.total;
        fs.complete = true;
        Some(fs)
    }

    /// Still waiting to be sized (or re-sized) in a job.
    fn is_pending(&self, folder: &Path, name: &OsStr) -> bool {
        self.jobs.iter().any(|j| j.folder == folder && j.todo.iter().any(|n| n == name))
    }

    /// Start a tree for `folder` with its own files; return the
    /// subfolders to size and the ones that are mounts.
    fn begin(&mut self, folder: &Path) -> Option<(VecDeque<OsString>, Vec<OsString>)> {
        let fd = repertoire_core::sys::open_dir(folder).ok()?;
        let st = repertoire_core::sys::stat_fd(fd.as_fd()).ok()?;
        let names = repertoire_core::sys::read_dir(fd.as_fd()).unwrap_or_default();
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut mounts = Vec::new();
        for n in names {
            let Ok(s) = repertoire_core::sys::stat_at(fd.as_fd(), &n) else { continue };
            let name = OsStr::from_bytes(&n).to_owned();
            if s.is_dir() {
                if s.dev == st.dev {
                    dirs.push(name);
                } else {
                    mounts.push(name);
                }
            } else {
                files.push(repertoire_core::scan::ScanFile { name: n, stat: s });
            }
        }
        let mut tree = Tree::new();
        let scan = repertoire_core::ScanDir { name: folder.as_os_str().as_bytes().into(), stat: st, handle: None, unreadable: false, files, dirs: Vec::new() };
        tree.graft(ROOT, scan, false);
        self.keep(Kept { root: folder.to_path_buf(), tree, complete: false, made: Instant::now() });
        // Send what's known right away: the files here.
        let k = self.kept.iter().find(|k| k.root == folder)?;
        let m = k.tree.mounts()[0];
        let d = k.tree.dir(m)?;
        self.send(SizeMsg::Done { folder: folder.to_path_buf(), total: d.total, files_here: d.own, hidden: 0, complete: dirs.is_empty() && mounts.is_empty() });
        let rank = |n: &OsString| self.order.iter().position(|o| o == n).unwrap_or(usize::MAX);
        dirs.sort_by_key(rank);
        Some((dirs.into(), mounts))
    }

    /// Re-read what's directly in `folder` after a change: its files, and
    /// subfolders that are gone. Subfolders themselves are re-measured by
    /// the job.
    fn refresh_files(&mut self, folder: &Path) {
        let Some(k) = self.kept.iter_mut().find(|k| k.root == folder) else { return };
        let Ok(fd) = repertoire_core::sys::open_dir(folder) else { return };
        let Ok(st) = repertoire_core::sys::stat_fd(fd.as_fd()) else { return };
        let mut now = std::collections::HashMap::new();
        for n in repertoire_core::sys::read_dir(fd.as_fd()).unwrap_or_default() {
            if let Ok(s) = repertoire_core::sys::stat_at(fd.as_fd(), &n) {
                now.insert(n, s);
            }
        }
        let tree = &mut k.tree;
        let m = tree.mounts()[0];
        for c in tree.children(m).to_vec() {
            let node = tree.node(c);
            let keep = node.is_dir() && now.get(&node.name).is_some_and(|s| s.is_dir() && s.ino == node.ino);
            if !keep {
                tree.remove(c);
            }
        }
        for (n, s) in &now {
            if !s.is_dir() {
                tree.insert_file(m, n, s, false);
            }
        }
        tree.update_dir(m, &st);
        let d = tree.dir(m).unwrap();
        let (total, own) = (d.total, d.own);
        self.send(SizeMsg::Done { folder: folder.to_path_buf(), total, files_here: own, hidden: 0, complete: false });
    }

    fn keep(&mut self, k: Kept) {
        self.kept.retain(|o| o.root != k.root);
        self.kept.push(k);
        while self.kept.len() > KEEP_TREES || self.kept.iter().map(|k| k.tree.len()).sum::<usize>() > KEEP_ENTRIES {
            // Drop the oldest that isn't the one being built.
            if self.kept.len() <= 1 {
                break;
            }
            self.kept.remove(0);
        }
    }

    /// Size the next subfolder of the first job.
    fn step(&mut self) {
        self.send(SizeMsg::State(SourceState::OwnScan { busy: true }));
        let Some(job) = self.jobs.front_mut() else { return };
        if job.cancel.load(Ordering::Relaxed) {
            self.jobs.pop_front();
            return;
        }
        let folder = job.folder.clone();
        let Some(name) = job.todo.pop_front() else {
            let mounts = std::mem::take(&mut job.mounts);
            for name in mounts {
                if let Some(size) = self.size_mount(&folder.join(&name)) {
                    self.send(SizeMsg::Child { folder: folder.clone(), name, size });
                }
            }
            self.finish(&folder);
            self.jobs.pop_front();
            return;
        };
        let cancel = job.cancel.clone();
        let scanner = Scanner { cancel: cancel.clone(), ..Scanner::default() };
        let Ok(fd) = repertoire_core::sys::open_dir(&folder) else {
            self.jobs.pop_front();
            return;
        };
        let Ok(st) = repertoire_core::sys::stat_fd(fd.as_fd()) else { return };
        let scan = self.pool.install(|| scanner.scan_at(fd.as_fd(), &folder, name.as_bytes(), st.dev));
        if cancel.load(Ordering::Relaxed) {
            return; // incomplete: dropped with the job next step
        }
        let Some(k) = self.kept.iter_mut().find(|k| k.root == folder) else { return };
        let m = k.tree.mounts()[0];
        let size = match scan {
            Some(scan) => {
                let id = k.tree.graft(m, scan, false);
                let d = k.tree.dir(id).unwrap();
                ChildSize { total: d.total, files: d.files, dirs: d.dirs, mtime: k.tree.node(id).mtime, unreadable: k.tree.is_unreadable(id), mount: None }
            }
            None => {
                // Gone (or became something else) meanwhile.
                if let Some(id) = k.tree.child(m, name.as_bytes()) {
                    k.tree.remove(id);
                }
                return;
            }
        };
        self.send(SizeMsg::Child { folder, name, size });
        self.answer_waiting();
    }

    /// Expanded folders whose part of the tree is now sized.
    fn answer_waiting(&mut self) {
        let waiting = std::mem::take(&mut self.waiting);
        for p in waiting {
            match self.sizes_in_kept(&p) {
                Some(sizes) => self.send(SizeMsg::Folder(sizes)),
                None => self.waiting.push(p),
            }
        }
    }

    fn finish(&mut self, folder: &Path) {
        let Some(k) = self.kept.iter_mut().find(|k| k.root == folder) else { return };
        k.tree.flush_links(&|_, _| None, true);
        k.complete = true;
        let m = k.tree.mounts()[0];
        let d = k.tree.dir(m).unwrap();
        let (total, own) = (d.total, d.own);
        // Hard links shared by subfolders were charged to the first one
        // sized; send everyone's final numbers.
        let children: Vec<(OsString, ChildSize)> = k
            .tree
            .children(m)
            .iter()
            .filter_map(|&c| {
                let cd = k.tree.dir(c)?;
                Some((k.tree.node(c).name().to_owned(), ChildSize { total: cd.total, files: cd.files, dirs: cd.dirs, mtime: k.tree.node(c).mtime, unreadable: k.tree.is_unreadable(c), mount: None }))
            })
            .collect();
        for (name, size) in children {
            self.send(SizeMsg::Child { folder: folder.to_path_buf(), name, size });
        }
        self.send(SizeMsg::Done { folder: folder.to_path_buf(), total, files_here: own, hidden: 0, complete: true });
        self.answer_waiting();
    }

    /// A mounted filesystem's total, with its own tree (cached).
    fn size_mount(&self, path: &Path) -> Option<ChildSize> {
        let scanner = Scanner { cancel: self.cancel_slot.lock().unwrap().clone(), ..Scanner::default() };
        let scan = self.pool.install(|| scanner.scan_root(path)).ok()?;
        if scanner.is_cancelled() {
            return None;
        }
        let mut t = Tree::new();
        let id = t.graft(ROOT, scan, false);
        let d = t.dir(id)?;
        Some(ChildSize { total: d.total, files: d.files, dirs: d.dirs, mtime: t.node(id).mtime, unreadable: t.is_unreadable(id), mount: Some(path.display().to_string()) })
    }
}

/// Subfolders of `folder` on another device than `dev`.
fn mounts_in(folder: &Path, dev: u64) -> Vec<(OsString, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    let Ok(rd) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut seen = HashSet::new();
    rd.flatten()
        .filter_map(|e| {
            let md = std::fs::symlink_metadata(e.path()).ok()?;
            (md.is_dir() && md.dev() != dev && seen.insert(e.file_name())).then(|| (e.file_name(), e.path()))
        })
        .collect()
}
