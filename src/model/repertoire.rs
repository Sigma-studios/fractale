//! Sizes from the répertoire daemon, the way Sentinelle's Storage view uses
//! it: try `Hello` at start, use the daemon while it answers, look for it
//! again every minute while it doesn't. A worker thread does all the socket
//! work; the UI never waits on it.
//!
//! For the open folder and each expanded row it asks `Children` (subfolder
//! totals, mount and unreadable flags, hidden bytes), then subscribes to
//! them: on each `Changed` it asks again. A folder the daemon doesn't index
//! (tmpfs, a small USB stick) is reported as such, so the explorer sizes it
//! itself.

use super::sizes::{ChildSize, FolderSizes, RepertoireState, SizeMsg, SourceState};
use repertoire_proto::{self as rp, Client, ErrorCode, FsState, Response};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Subscribed paths at most (the open folder and expanded rows).
const MAX_SUBSCRIBED: usize = 50;

pub enum Msg {
    /// The daemon answers (again).
    Up,
    /// The daemon went away (or never was there).
    Down,
    Size(SizeMsg),
    /// The daemon doesn't index this folder.
    NotIndexed(PathBuf),
    Top { path: PathBuf, folders: Vec<(PathBuf, u64)> },
}

enum Input {
    Watch { folder: PathBuf, expanded: Vec<PathBuf> },
    Top { path: PathBuf, n: u32 },
    Rescan,
    /// From the subscription thread: a watched folder (daemon path) changed.
    Changed { generation: u64, path: PathBuf },
    State { generation: u64, fs_id: String, state: FsState },
    SubscriptionEnded { generation: u64 },
}

pub struct Repertoire {
    tx: Sender<Input>,
    pub rx: Receiver<Msg>,
}

impl Repertoire {
    pub fn spawn(socket: PathBuf, retry: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> Repertoire {
        let (tx, inputs) = channel();
        let (out, rx) = channel();
        let me = tx.clone();
        std::thread::Builder::new()
            .name("repertoire".into())
            .spawn(move || Worker { socket, retry, out, wake, me, client: None, watched: Vec::new(), real: HashMap::new(), subscription: None, generation: 0, fs_id: None }.run(inputs))
            .expect("spawn repertoire client");
        Repertoire { tx, rx }
    }

    pub fn watch(&self, folder: &Path, expanded: Vec<PathBuf>) {
        let _ = self.tx.send(Input::Watch { folder: folder.to_path_buf(), expanded });
    }

    pub fn top(&self, path: &Path, n: u32) {
        let _ = self.tx.send(Input::Top { path: path.to_path_buf(), n });
    }

    pub fn rescan(&self) {
        let _ = self.tx.send(Input::Rescan);
    }
}

struct Worker {
    socket: PathBuf,
    retry: Duration,
    out: Sender<Msg>,
    wake: Arc<dyn Fn() + Send + Sync>,
    me: Sender<Input>,
    client: Option<Client>,
    /// What's shown: the open folder first, then expanded rows.
    watched: Vec<PathBuf>,
    /// Shown path -> the daemon's (real) path. Folders can be reached
    /// through symlinks; the daemon only knows real paths.
    real: HashMap<PathBuf, PathBuf>,
    /// The subscription connection (shut down to replace it).
    subscription: Option<std::os::unix::net::UnixStream>,
    generation: u64,
    /// Filesystem of the open folder, for the state badge.
    fs_id: Option<String>,
}

impl Worker {
    fn send(&self, m: Msg) {
        let _ = self.out.send(m);
        (self.wake)();
    }

    fn run(mut self, inputs: Receiver<Input>) {
        let mut next_try = Instant::now();
        loop {
            if self.client.is_none() && Instant::now() >= next_try {
                next_try = Instant::now() + self.retry;
                if self.connect() {
                    self.send(Msg::Up);
                    self.refresh_all();
                } else {
                    self.send(Msg::Down);
                }
            }
            let wait = if self.client.is_some() { Duration::from_secs(3600) } else { next_try.saturating_duration_since(Instant::now()) };
            let input = match inputs.recv_timeout(wait) {
                Ok(i) => i,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let ok = self.handle(input);
            if !ok {
                self.disconnect();
                self.send(Msg::Down);
                next_try = Instant::now() + self.retry;
            }
        }
    }

    fn connect(&mut self) -> bool {
        match Client::connect_to(&self.socket).and_then(|mut c| c.hello("fractale").map(|_| c)) {
            Ok(c) => {
                self.client = Some(c);
                true
            }
            Err(_) => false,
        }
    }

    fn disconnect(&mut self) {
        self.client = None;
        if let Some(s) = self.subscription.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// False when the connection broke.
    fn handle(&mut self, input: Input) -> bool {
        match input {
            Input::Watch { folder, expanded } => {
                let mut watched = vec![folder];
                watched.extend(expanded);
                watched.truncate(MAX_SUBSCRIBED);
                let same = watched == self.watched;
                self.watched = watched;
                if self.client.is_none() {
                    return true;
                }
                if !same {
                    self.refresh_all() && self.resubscribe()
                } else {
                    true
                }
            }
            Input::Top { path, n } => {
                let Some(c) = self.client.as_mut() else { return true };
                let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                match c.top(&real, n) {
                    Ok(folders) => {
                        let folders = folders.into_iter().map(|t| (rebase(&t.path.to_path(), &real, &path), t.total)).collect();
                        self.send(Msg::Top { path, folders });
                        true
                    }
                    Err(rp::Error::Daemon { .. }) => true,
                    Err(_) => false,
                }
            }
            Input::Rescan => {
                let Some(c) = self.client.as_mut() else { return true };
                !matches!(c.rescan(None), Err(rp::Error::Io(_)))
            }
            Input::Changed { generation, path } => {
                if generation != self.generation {
                    return true;
                }
                // Map the daemon's path back to what we show.
                let shown: Vec<PathBuf> = self.real.iter().filter(|(_, r)| **r == path).map(|(s, _)| s.clone()).collect();
                for s in shown {
                    if !self.fetch(&s) {
                        return false;
                    }
                }
                true
            }
            Input::State { generation, fs_id, state } => {
                if generation == self.generation && self.fs_id.as_deref() == Some(&fs_id) {
                    self.send(Msg::Size(SizeMsg::State(SourceState::Repertoire(map_state(&state)))));
                }
                true
            }
            Input::SubscriptionEnded { generation } => generation != self.generation || self.client.is_none(),
        }
    }

    fn refresh_all(&mut self) -> bool {
        self.fs_id = None;
        for p in self.watched.clone() {
            if !self.fetch(&p) {
                return false;
            }
        }
        // The open folder's filesystem and its state.
        if let (Some(open), Some(c)) = (self.watched.first().cloned(), self.client.as_mut()) {
            let real = self.real.get(&open).cloned().unwrap_or(open);
            match c.filesystems() {
                Ok(fss) => {
                    let fs = fss
                        .iter()
                        .filter_map(|f| f.mount_points.iter().map(|m| m.path.to_path()).filter(|m| real.starts_with(m)).map(|m| m.as_os_str().len()).max().map(|l| (l, f)))
                        .max_by_key(|(l, _)| *l)
                        .map(|(_, f)| f);
                    if let Some(f) = fs {
                        self.fs_id = Some(f.id.clone());
                        self.send(Msg::Size(SizeMsg::State(SourceState::Repertoire(map_state(&f.state)))));
                    }
                }
                Err(rp::Error::Io(_)) => return false,
                Err(_) => {}
            }
        }
        self.resubscribe()
    }

    /// Ask `Children` for one shown folder. False if the connection broke.
    fn fetch(&mut self, shown: &Path) -> bool {
        let real = std::fs::canonicalize(shown).unwrap_or_else(|_| shown.to_path_buf());
        self.real.insert(shown.to_path_buf(), real.clone());
        let Some(c) = self.client.as_mut() else { return true };
        match c.children(&real, false, rp::Sort::Size, 1_000_000, 0) {
            Ok(l) => {
                self.send(Msg::Size(SizeMsg::Folder(folder_sizes(shown, &l))));
                true
            }
            Err(rp::Error::Daemon { code: ErrorCode::NotFound, .. }) => {
                self.send(Msg::NotIndexed(shown.to_path_buf()));
                true
            }
            Err(rp::Error::Daemon { .. }) => true,
            Err(_) => false,
        }
    }

    /// Replace the subscription with one for what's watched now.
    fn resubscribe(&mut self) -> bool {
        if let Some(s) = self.subscription.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        self.generation += 1;
        let paths: Vec<PathBuf> = self.watched.iter().map(|p| self.real.get(p).cloned().unwrap_or_else(|| p.clone())).collect();
        if paths.is_empty() {
            return true;
        }
        // Our own stream (rather than `Client::subscribe`), so it can be shut
        // down from here when what's watched changes.
        let Ok(stream) = std::os::unix::net::UnixStream::connect(&self.socket) else { return false };
        let req = rp::Envelope::new(rp::Request::Subscribe { paths: paths.iter().map(|p| rp::SubscribePath { path: p.as_path().into() }).collect() });
        let Ok(mut line) = serde_json::to_vec(&req) else { return false };
        line.push(b'\n');
        use std::io::Write;
        if (&stream).write_all(&line).is_err() {
            return false;
        }
        self.subscription = stream.try_clone().ok();
        let (me, generation) = (self.me.clone(), self.generation);
        std::thread::Builder::new()
            .name("repertoire-sub".into())
            .spawn(move || {
                use std::io::BufRead;
                // The first `Changed` per path reports the current total,
                // which was just fetched.
                let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
                for l in std::io::BufReader::new(stream).lines() {
                    let Ok(l) = l else { break };
                    let Ok(env) = serde_json::from_str::<rp::Envelope<Response>>(&l) else { continue };
                    match env.body {
                        Response::Changed { path, .. } => {
                            let p = path.to_path();
                            if seen.insert(p.clone()) {
                                continue;
                            }
                            if me.send(Input::Changed { generation, path: p }).is_err() {
                                return;
                            }
                        }
                        Response::FsState { fs_id, state } => {
                            let _ = me.send(Input::State { generation, fs_id, state });
                        }
                        _ => {}
                    }
                }
                let _ = me.send(Input::SubscriptionEnded { generation });
            })
            .is_ok()
    }
}

/// `path` (under the daemon's `real` root) as seen under `shown`.
fn rebase(path: &Path, real: &Path, shown: &Path) -> PathBuf {
    match path.strip_prefix(real) {
        Ok(rest) => shown.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

pub fn map_state(s: &FsState) -> RepertoireState {
    match *s {
        FsState::Live => RepertoireState::Live,
        FsState::Static => RepertoireState::Static,
        FsState::Reconciling { progress, .. } => RepertoireState::Reconciling { progress },
        FsState::Scanning { progress, .. } => RepertoireState::Scanning { progress },
        FsState::Stale { progress, .. } => RepertoireState::Stale { progress },
    }
}

/// A `Children` answer as the sizes of `shown`.
pub fn folder_sizes(shown: &Path, l: &rp::Listing) -> FolderSizes {
    let mut fs = FolderSizes::new(shown.to_path_buf());
    for d in &l.dirs {
        let name = OsString::from_vec(d.name.bytes().to_vec());
        fs.children.insert(name, ChildSize { total: d.total, files: d.files, dirs: d.dirs, mtime: d.mtime, unreadable: d.unreadable, mount: d.mount.clone() });
    }
    fs.total = l.total;
    fs.hidden = l.hidden_bytes;
    fs.files_here = l.total.saturating_sub(l.hidden_bytes).saturating_sub(fs.children_total());
    fs.complete = true;
    fs
}
