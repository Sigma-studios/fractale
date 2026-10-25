//! Search by name under the open folder (or everywhere): répertoire's
//! `Search` when it runs, otherwise a background walk that streams matches.
//! Both rank the same way (exact name, prefix, word start, substring; then
//! shorter names, then larger sizes), with `repertoire_core::query::rank`.

use super::entry::Kind;
use repertoire_core::Rank;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const LIMIT: usize = 500;
/// The walk stops after this many entries or this long, and says so.
const WALK_ENTRIES: u64 = 400_000;
const WALK_TIME: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub kind: Kind,
    /// A file's allocated bytes, or a folder's total (répertoire only).
    pub bytes: Option<u64>,
    pub rank: Rank,
}

impl Hit {
    fn key(&self) -> (Rank, usize, std::cmp::Reverse<u64>, PathBuf) {
        let len = self.path.file_name().map_or(0, |n| n.len());
        (self.rank, len, std::cmp::Reverse(self.bytes.unwrap_or(0)), self.path.clone())
    }
}

#[derive(Clone, Debug)]
pub struct Query {
    pub text: String,
    /// Where to look; `None`: everywhere.
    pub under: Option<PathBuf>,
    pub show_hidden: bool,
}

pub struct Results {
    pub generation: u64,
    pub hits: Vec<Hit>,
    pub done: bool,
    /// The walk stopped early: there may be more.
    pub partial: bool,
    /// Answered by répertoire.
    pub indexed: bool,
}

pub struct Searcher {
    tx: Sender<(u64, Query, bool)>,
    pub rx: Receiver<Results>,
    latest: Arc<AtomicU64>,
    generation: u64,
}

impl Searcher {
    pub fn spawn(socket: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Searcher {
        let (tx, jobs) = channel::<(u64, Query, bool)>();
        let (out, rx) = channel();
        let latest = Arc::new(AtomicU64::new(0));
        let l = latest.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                while let Ok(mut job) = jobs.recv() {
                    while let Ok(newer) = jobs.try_recv() {
                        job = newer;
                    }
                    let (generation, query, daemon) = job;
                    let send = |r: Results| {
                        let _ = out.send(r);
                        wake();
                    };
                    if daemon {
                        if let Some(hits) = ask_daemon(&socket, &query) {
                            send(Results { generation, hits, done: true, partial: false, indexed: true });
                            continue;
                        }
                    }
                    walk(&query, generation, &l, &send);
                }
            })
            .expect("spawn searcher");
        Searcher { tx, rx, latest, generation: 0 }
    }

    /// Start a search (cancels the one running). Returns its generation.
    pub fn search(&mut self, query: Query, use_daemon: bool) -> u64 {
        self.generation += 1;
        self.latest.store(self.generation, Ordering::Relaxed);
        let _ = self.tx.send((self.generation, query, use_daemon));
        self.generation
    }

    pub fn cancel(&mut self) {
        self.generation += 1;
        self.latest.store(self.generation, Ordering::Relaxed);
    }
}

/// Names that are hidden somewhere between `under` and the hit.
fn hidden_below(path: &Path, under: Option<&Path>) -> bool {
    let rest = under.and_then(|u| path.strip_prefix(u).ok()).unwrap_or(path);
    rest.components().any(|c| c.as_os_str().as_bytes().first() == Some(&b'.'))
}

fn ask_daemon(socket: &Path, q: &Query) -> Option<Vec<Hit>> {
    let mut c = repertoire_proto::Client::connect_to(socket).ok()?;
    // The daemon knows real paths; map results back under what's shown.
    let real = q.under.as_ref().map(|u| std::fs::canonicalize(u).unwrap_or_else(|_| u.clone()));
    let found = c.search(&q.text, real.as_deref(), LIMIT as u32 * 2, &[]).ok()?;
    let mut hits: Vec<Hit> = found
        .into_iter()
        .map(|h| {
            let p = h.path.to_path();
            let path = match (&real, &q.under) {
                (Some(r), Some(u)) => p.strip_prefix(r).map(|rest| u.join(rest)).unwrap_or(p),
                _ => p,
            };
            let kind = match h.kind {
                repertoire_proto::Kind::Dir => Kind::Dir,
                repertoire_proto::Kind::File => Kind::File,
                repertoire_proto::Kind::Symlink => Kind::Symlink,
                repertoire_proto::Kind::Other => Kind::Other,
            };
            let rank = match h.rank {
                repertoire_proto::Rank::Exact => Rank::Exact,
                repertoire_proto::Rank::Prefix => Rank::Prefix,
                repertoire_proto::Rank::Word => Rank::WordStart,
                repertoire_proto::Rank::Substring => Rank::Substring,
            };
            Hit { path, kind, bytes: Some(h.bytes), rank }
        })
        .filter(|h| q.show_hidden || !hidden_below(&h.path, q.under.as_deref()))
        .collect();
    hits.truncate(LIMIT);
    Some(hits)
}

/// Walk breadth-first, keep the best `LIMIT`, report every ~100 ms.
fn walk(q: &Query, generation: u64, latest: &AtomicU64, send: &dyn Fn(Results)) {
    let root = q.under.clone().unwrap_or_else(|| "/".into());
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut best: Vec<Hit> = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.clone()]);
    let mut seen = 0u64;
    let mut partial = false;
    let skip: &[&str] = &["/proc", "/sys", "/dev", "/run"];
    'outer: while let Some(dir) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if latest.load(Ordering::Relaxed) != generation {
                return;
            }
            seen += 1;
            if seen >= WALK_ENTRIES || start.elapsed() >= WALK_TIME {
                partial = true;
                break 'outer;
            }
            let name = e.file_name();
            let hidden = name.as_bytes().first() == Some(&b'.');
            if hidden && !q.show_hidden {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() && !skip.iter().any(|s| path == Path::new(s)) {
                queue.push_back(path.clone());
            }
            let Some(rank) = repertoire_core::query::rank(&q.text, name.as_bytes()) else { continue };
            let (kind, bytes) = if ft.is_dir() {
                (Kind::Dir, None)
            } else {
                use std::os::unix::fs::MetadataExt;
                let md = std::fs::symlink_metadata(&path).ok();
                let kind = if ft.is_symlink() {
                    Kind::Symlink
                } else if ft.is_file() {
                    Kind::File
                } else {
                    Kind::Other
                };
                (kind, md.map(|m| m.blocks() * 512))
            };
            best.push(Hit { path, kind, bytes, rank });
            if best.len() > LIMIT * 2 {
                best.sort_by_key(Hit::key);
                best.truncate(LIMIT);
            }
            if last_report.elapsed() > Duration::from_millis(100) {
                last_report = Instant::now();
                best.sort_by_key(Hit::key);
                best.truncate(LIMIT);
                send(Results { generation, hits: best.clone(), done: false, partial: false, indexed: false });
            }
        }
    }
    best.sort_by_key(Hit::key);
    best.truncate(LIMIT);
    send(Results { generation, hits: best, done: true, partial, indexed: false });
}
