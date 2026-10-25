//! Changing files: copy, move, trash and delete as queued jobs with
//! progress, conflicts, errors and cancel; rename, new folder and counting
//! as quick operations. All of it runs on worker threads; the UI submits
//! and answers questions over channels.
//!
//! Jobs run one at a time per destination filesystem (two copies to the
//! same disk would only fight over it) and in parallel across filesystems.

mod work;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type JobId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
    Trash,
    Delete,
}

impl Kind {
    pub fn verb(self) -> &'static str {
        match self {
            Kind::Copy => "Copying",
            Kind::Move => "Moving",
            Kind::Trash => "Moving to the trash",
            Kind::Delete => "Deleting",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub kind: Kind,
    pub sources: Vec<PathBuf>,
    /// The folder to copy or move into.
    pub dest: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub items_done: u64,
    pub items_total: u64,
    /// Still finding out how much there is.
    pub counting: bool,
    pub current: PathBuf,
    pub started: Option<Instant>,
}

impl Progress {
    /// Time left from the rate so far (bytes, or items when there are no
    /// bytes to speak of).
    pub fn left(&self) -> Option<Duration> {
        let el = self.started?.elapsed().as_secs_f64();
        let (done, total) = if self.bytes_total > 0 { (self.bytes_done, self.bytes_total) } else { (self.items_done, self.items_total) };
        if self.counting || done == 0 || el < 1.0 || total <= done {
            return None;
        }
        Some(Duration::from_secs_f64(el * (total - done) as f64 / done as f64))
    }
}

/// What's known about one side of a conflict.
#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Replace,
    Skip,
    /// Copy under a new name: "name (2).ext".
    KeepBoth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorChoice {
    Retry,
    Skip,
    Cancel,
}

/// A question a job waits on.
#[derive(Clone, Debug)]
pub enum Question {
    Conflict { source: PathBuf, target: PathBuf, source_brief: Brief, target_brief: Brief },
    Error { path: PathBuf, what: String, error: String },
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub done: u64,
    pub skipped: u64,
    pub cancelled: bool,
    /// What now exists at the destination, top level (to select it).
    pub created: Vec<PathBuf>,
}

/// A job as the UI sees it.
#[derive(Clone, Debug)]
pub struct JobView {
    pub id: JobId,
    pub spec: Spec,
    pub progress: Progress,
    pub question: Option<Question>,
    /// Waiting for another job on the same filesystem.
    pub queued: bool,
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Debug)]
pub enum Quick {
    Rename { path: PathBuf, name: OsString },
    NewFolder { parent: PathBuf },
    /// Items and allocated bytes under some paths (for the delete
    /// confirmation).
    Count { paths: Vec<PathBuf> },
}

#[derive(Clone, Debug)]
pub enum QuickDone {
    Renamed { from: PathBuf, to: PathBuf },
    Created(PathBuf),
    Counted { paths: Vec<PathBuf>, items: u64, bytes: u64 },
    Failed { what: String, error: String },
}

pub enum Msg {
    /// A job changed (progress, question, finished).
    Job(JobId),
    Quick(QuickDone),
}

/// Answers from the UI to a running job.
enum Answer {
    Conflict(Resolution, bool),
    Error(ErrorChoice),
}

struct Running {
    view: JobView,
    cancel: Arc<AtomicBool>,
    answers: Option<Sender<Answer>>,
    /// The filesystem it's queued on.
    lane: Option<u64>,
}

struct State {
    jobs: HashMap<JobId, Running>,
    next: JobId,
    /// Jobs per filesystem, in order; the first runs.
    lanes: HashMap<u64, Vec<JobId>>,
}

pub struct Jobs {
    state: Arc<Mutex<State>>,
    pub rx: Receiver<Msg>,
    tx: Sender<Msg>,
    quick: Sender<Quick>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Jobs {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> Jobs {
        let (tx, rx) = channel();
        let (quick, quick_rx) = channel::<Quick>();
        let (out, w) = (tx.clone(), wake.clone());
        std::thread::Builder::new()
            .name("quick-ops".into())
            .spawn(move || {
                while let Ok(q) = quick_rx.recv() {
                    let _ = out.send(Msg::Quick(work::quick(q)));
                    w();
                }
            })
            .expect("spawn quick ops");
        Jobs { state: Arc::new(Mutex::new(State { jobs: HashMap::new(), next: 1, lanes: HashMap::new() })), rx, tx, quick, wake }
    }

    pub fn quick(&self, q: Quick) {
        let _ = self.quick.send(q);
    }

    /// Queue a job. It starts when its filesystem is free.
    pub fn submit(&self, spec: Spec) -> JobId {
        let id = {
            let mut st = self.state.lock().unwrap();
            let id = st.next;
            st.next += 1;
            let view = JobView { id, spec: spec.clone(), progress: Progress { counting: true, ..Default::default() }, question: None, queued: true, outcome: None };
            st.jobs.insert(id, Running { view, cancel: Arc::new(AtomicBool::new(false)), answers: None, lane: None });
            id
        };
        // Which filesystem it lands on needs a stat: not on the caller's
        // (UI) thread.
        let (state, tx, wake) = (self.state.clone(), self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            use std::os::unix::fs::MetadataExt;
            let at = spec.dest.as_ref().or(spec.sources.first());
            let lane = at.and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.dev());
            let start = {
                let mut st = state.lock().unwrap();
                if let Some(r) = st.jobs.get_mut(&id) {
                    r.lane = Some(lane);
                }
                let q = st.lanes.entry(lane).or_default();
                q.push(id);
                q.len() == 1
            };
            if start {
                run(state, tx, wake, id);
            }
        });
        id
    }

    pub fn cancel(&self, id: JobId) {
        let st = self.state.lock().unwrap();
        if let Some(r) = st.jobs.get(&id) {
            r.cancel.store(true, Ordering::Relaxed);
            // A job waiting on a question is told directly.
            if let Some(a) = &r.answers {
                let _ = a.send(Answer::Error(ErrorChoice::Cancel));
            }
        }
    }

    pub fn resolve(&self, id: JobId, r: Resolution, all: bool) {
        self.answer(id, Answer::Conflict(r, all));
    }

    pub fn error_choice(&self, id: JobId, c: ErrorChoice) {
        self.answer(id, Answer::Error(c));
    }

    fn answer(&self, id: JobId, a: Answer) {
        let mut st = self.state.lock().unwrap();
        if let Some(r) = st.jobs.get_mut(&id) {
            r.view.question = None;
            if let Some(tx) = &r.answers {
                let _ = tx.send(a);
            }
        }
    }

    /// Every job not yet dismissed, oldest first.
    pub fn views(&self) -> Vec<JobView> {
        let st = self.state.lock().unwrap();
        let mut v: Vec<JobView> = st.jobs.values().map(|r| r.view.clone()).collect();
        v.sort_by_key(|j| j.id);
        v
    }

    pub fn view(&self, id: JobId) -> Option<JobView> {
        self.state.lock().unwrap().jobs.get(&id).map(|r| r.view.clone())
    }

    /// Forget a finished job.
    pub fn dismiss(&self, id: JobId) {
        let mut st = self.state.lock().unwrap();
        if st.jobs.get(&id).is_some_and(|r| r.view.outcome.is_some()) {
            st.jobs.remove(&id);
        }
    }

    pub fn busy(&self) -> bool {
        self.state.lock().unwrap().jobs.values().any(|r| r.view.outcome.is_none())
    }
}

/// Run a job on this thread, then the next one queued on its filesystem.
fn run(state: Arc<Mutex<State>>, tx: Sender<Msg>, wake: Arc<dyn Fn() + Send + Sync>, mut id: JobId) {
    loop {
        let (answers_tx, answers) = channel();
        let (spec, cancel) = {
            let mut st = state.lock().unwrap();
            let r = st.jobs.get_mut(&id).expect("queued job");
            r.answers = Some(answers_tx);
            r.view.queued = false;
            r.view.progress.started = Some(Instant::now());
            (r.view.spec.clone(), r.cancel.clone())
        };
        let ctx = work::Ctx { id, state: &state, tx: &tx, wake: &*wake, cancel: &cancel, answers: &answers, last_report: std::cell::Cell::new(None) };
        let outcome = work::run(&ctx, &spec);
        let next = {
            let mut st = state.lock().unwrap();
            let lane = st.jobs.get(&id).and_then(|r| r.lane);
            if let Some(r) = st.jobs.get_mut(&id) {
                r.view.outcome = Some(outcome);
                r.view.question = None;
                r.answers = None;
            }
            let q = lane.and_then(|l| st.lanes.get_mut(&l));
            q.and_then(|q| {
                q.retain(|j| *j != id);
                q.first().copied()
            })
        };
        let _ = tx.send(Msg::Job(id));
        wake();
        match next {
            Some(n) => id = n,
            None => return,
        }
    }
}

/// "name (2).ext", "name (3).ext", … : the first that doesn't exist.
pub fn free_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let b = name.as_bytes();
    // The extension stays at the end; a leading dot isn't one.
    let split = b.iter().rposition(|&c| c == b'.').filter(|&i| i > 0).unwrap_or(b.len());
    let (stem, ext) = b.split_at(split);
    for n in 2.. {
        let mut v = stem.to_vec();
        v.extend(format!(" ({n})").as_bytes());
        v.extend(ext);
        let p = dir.join(OsString::from_vec(v));
        if std::fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}
