//! What a job does, on its own thread.
//!
//! Copies are written to a hidden part file next to the target and renamed
//! into place once complete, so a cancelled or failed copy never leaves a
//! partial file under the real name, and "replace" swaps atomically. A move
//! on one filesystem is a rename; across filesystems each file is copied,
//! synced to disk and checked, and only then is its source removed.

use super::*;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub struct Ctx<'a> {
    pub id: JobId,
    pub state: &'a Arc<Mutex<State>>,
    pub tx: &'a Sender<Msg>,
    pub wake: &'a (dyn Fn() + Send + Sync),
    pub cancel: &'a AtomicBool,
    pub answers: &'a Receiver<Answer>,
    pub last_report: std::cell::Cell<Option<Instant>>,
}

/// Progress reports at most this often.
const REPORT_EVERY: Duration = Duration::from_millis(50);

impl Ctx<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn progress(&self, f: impl FnOnce(&mut Progress)) {
        {
            let mut st = self.state.lock().unwrap();
            if let Some(r) = st.jobs.get_mut(&self.id) {
                f(&mut r.view.progress);
            }
        }
        let now = Instant::now();
        if self.last_report.get().is_none_or(|t| now - t >= REPORT_EVERY) {
            self.last_report.set(Some(now));
            self.notify();
        }
    }

    fn notify(&self) {
        let _ = self.tx.send(Msg::Job(self.id));
        (self.wake)();
    }

    fn ask(&self, q: Question) -> Answer {
        if let Some(r) = self.state.lock().unwrap().jobs.get_mut(&self.id) {
            r.view.question = Some(q);
        }
        self.notify();
        let a = self.answers.recv().unwrap_or(Answer::Error(ErrorChoice::Cancel));
        self.notify();
        a
    }

    /// Run `f` until it works, the user skips (None) or cancels (None, and
    /// the job stops).
    fn attempt<T>(&self, path: &Path, what: &str, mut f: impl FnMut() -> io::Result<T>) -> Option<T> {
        loop {
            if self.cancelled() {
                return None;
            }
            match f() {
                Ok(v) => return Some(v),
                Err(e) if self.cancelled() && e.kind() == io::ErrorKind::Interrupted => return None,
                Err(e) => match self.ask(Question::Error { path: path.to_path_buf(), what: what.into(), error: e.to_string() }) {
                    Answer::Error(ErrorChoice::Retry) => continue,
                    Answer::Error(ErrorChoice::Skip) | Answer::Conflict(..) => return None,
                    Answer::Error(ErrorChoice::Cancel) => {
                        self.cancel.store(true, Ordering::Relaxed);
                        return None;
                    }
                },
            }
        }
    }
}

pub fn run(ctx: &Ctx, spec: &Spec) -> Outcome {
    let mut out = match spec.kind {
        Kind::Copy | Kind::Move => Transfer::new(ctx, spec).map(|t| t.run(spec)).unwrap_or_default(),
        Kind::Trash => trash(ctx, spec),
        Kind::Delete => delete(ctx, spec),
    };
    out.cancelled = ctx.cancelled();
    out
}

fn brief(md: &Metadata) -> Brief {
    Brief { is_dir: md.is_dir(), size: md.len(), mtime: md.mtime() }
}

/// Items and bytes (file lengths, for copy progress) under `p`.
fn measure(ctx: &Ctx, p: &Path) -> (u64, u64) {
    let Ok(md) = fs::symlink_metadata(p) else { return (1, 0) };
    if !md.is_dir() {
        return (1, if md.is_file() { md.len() } else { 0 });
    }
    let mut total = (1, 0);
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            if ctx.cancelled() {
                break;
            }
            let (i, b) = measure(ctx, &e.path());
            total.0 += i;
            total.1 += b;
        }
    }
    total
}

struct Transfer<'a, 'b> {
    ctx: &'a Ctx<'b>,
    moving: bool,
    dest_dev: u64,
    /// "Apply to all" answer.
    all: Option<Resolution>,
    out: Outcome,
}

impl<'a, 'b> Transfer<'a, 'b> {
    fn new(ctx: &'a Ctx<'b>, spec: &Spec) -> Option<Transfer<'a, 'b>> {
        let dest = spec.dest.as_ref()?;
        let md = ctx.attempt(dest, "Opening the destination", || fs::metadata(dest))?;
        Some(Transfer { ctx, moving: spec.kind == Kind::Move, dest_dev: md.dev(), all: None, out: Outcome::default() })
    }

    fn renames(&self, md: &Metadata) -> bool {
        self.moving && md.dev() == self.dest_dev
    }

    fn run(mut self, spec: &Spec) -> Outcome {
        let dest = spec.dest.clone().unwrap_or_default();
        // How much there is (a rename is one item, whatever it holds).
        let (mut items, mut bytes) = (0, 0);
        for s in &spec.sources {
            match fs::symlink_metadata(s) {
                Ok(md) if self.renames(&md) => items += 1,
                _ => {
                    let (i, b) = measure(self.ctx, s);
                    items += i;
                    bytes += b;
                }
            }
        }
        self.ctx.progress(|p| {
            p.items_total = items;
            p.bytes_total = bytes;
            p.counting = false;
        });
        for src in &spec.sources {
            if self.ctx.cancelled() {
                break;
            }
            let Some(name) = src.file_name() else { continue };
            let mut target = dest.join(name);
            if src.is_dir() && dest.starts_with(src) {
                let what = if self.moving { "A folder can't be moved into itself" } else { "A folder can't be copied into itself" };
                self.ctx.attempt(src, what, || Err::<(), _>(io::Error::other("it contains the destination")));
                self.out.skipped += 1;
                continue;
            }
            if target == *src {
                if self.moving {
                    // Already there.
                    self.out.created.push(target);
                    continue;
                }
                target = free_name(&dest, name);
            }
            if let Some(done) = self.item(src, target) {
                self.out.created.push(done);
            }
        }
        self.out
    }

    fn resolve(&mut self, src: &Path, target: &Path, md: &Metadata, tmd: &Metadata) -> Option<Resolution> {
        if let Some(r) = self.all {
            return Some(r);
        }
        let q = Question::Conflict { source: src.to_path_buf(), target: target.to_path_buf(), source_brief: brief(md), target_brief: brief(tmd) };
        match self.ctx.ask(q) {
            Answer::Conflict(r, all) => {
                if all {
                    self.all = Some(r);
                }
                Some(r)
            }
            Answer::Error(_) => {
                self.ctx.cancel.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    /// Copy or move one item (and what's in it); where it ended up.
    fn item(&mut self, src: &Path, target: PathBuf) -> Option<PathBuf> {
        let ctx = self.ctx;
        ctx.progress(|p| p.current = src.to_path_buf());
        let md = ctx.attempt(src, "Reading", || fs::symlink_metadata(src)).or_else(|| {
            self.out.skipped += 1;
            None
        })?;
        let mut target = target;
        let mut replacing = None;
        if let Ok(tmd) = fs::symlink_metadata(&target) {
            if md.is_dir() && tmd.is_dir() {
                // Folders merge; what's inside can conflict.
                return self.merge(src, target, &md);
            }
            match self.resolve(src, &target, &md, &tmd)? {
                Resolution::Skip => {
                    let (i, b) = if self.renames(&md) { (1, 0) } else { measure(ctx, src) };
                    ctx.progress(|p| {
                        p.items_done += i;
                        p.bytes_done += b;
                    });
                    self.out.skipped += 1;
                    return None;
                }
                Resolution::KeepBoth => target = free_name(target.parent()?, target.file_name()?),
                Resolution::Replace => replacing = Some(tmd),
            }
        }
        // A file replaces a file atomically (rename over it); anything
        // else is removed first.
        let clear = |t: &Path| -> io::Result<()> {
            match &replacing {
                Some(tmd) if tmd.is_dir() => fs::remove_dir_all(t),
                Some(_) if md.is_dir() => fs::remove_file(t),
                _ => Ok(()),
            }
        };
        if self.renames(&md) {
            ctx.attempt(src, "Moving", || {
                clear(&target)?;
                fs::rename(src, &target)
            })
            .or_else(|| {
                self.out.skipped += 1;
                None
            })?;
            ctx.progress(|p| p.items_done += 1);
            self.out.done += 1;
            return Some(target);
        }
        let ft = md.file_type();
        let ok = if ft.is_dir() {
            ctx.attempt(&target, "Making the folder", || {
                clear(&target)?;
                fs::create_dir(&target)
            })
            .is_some()
                && {
                    ctx.progress(|p| p.items_done += 1);
                    self.contents(src, &target);
                    finish_dir(&target, &md);
                    true
                }
        } else if ft.is_symlink() {
            ctx.attempt(src, "Copying the link", || {
                let to = fs::read_link(src)?;
                let part = part_name(&target);
                let _ = fs::remove_file(&part);
                std::os::unix::fs::symlink(to, &part)?;
                clear(&target)?;
                fs::rename(&part, &target)
            })
            .is_some()
        } else if ft.is_file() {
            self.file(src, &target, &md, &clear)
        } else {
            ctx.attempt(src, "Copying", || Err::<(), _>(io::Error::other("special files (pipes, sockets, devices) aren't copied")));
            false
        };
        if !ok {
            if !ctx.cancelled() {
                self.out.skipped += 1;
            }
            return None;
        }
        if !ft.is_dir() {
            ctx.progress(|p| p.items_done += 1);
        }
        self.out.done += 1;
        if self.moving {
            // The copy is complete and on disk: the source can go. A folder
            // only goes if everything in it went (anything skipped stays).
            let removed = if ft.is_dir() { fs::remove_dir(src) } else { fs::remove_file(src) };
            if removed.is_err() && !ft.is_dir() {
                ctx.attempt(src, "Removing the moved file", || fs::remove_file(src));
            }
        }
        Some(target)
    }

    fn merge(&mut self, src: &Path, target: PathBuf, md: &Metadata) -> Option<PathBuf> {
        self.ctx.progress(|p| p.items_done += 1);
        if self.renames(md) {
            // The source folder's contents move one by one; count them in.
            let (i, _) = measure(self.ctx, src);
            self.ctx.progress(|p| p.items_total += i.saturating_sub(1));
        }
        self.contents(src, &target);
        if self.moving {
            let _ = fs::remove_dir(src);
        }
        Some(target)
    }

    fn contents(&mut self, src: &Path, target: &Path) {
        let Some(rd) = self.ctx.attempt(src, "Reading the folder", || fs::read_dir(src)) else { return };
        let mut names: Vec<_> = rd.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for n in names {
            if self.ctx.cancelled() {
                return;
            }
            self.item(&src.join(&n), target.join(&n));
        }
    }

    fn file(&mut self, src: &Path, target: &Path, md: &Metadata, clear: &dyn Fn(&Path) -> io::Result<()>) -> bool {
        let ctx = self.ctx;
        let sync = self.moving;
        let mut counted = 0u64;
        ctx.attempt(src, "Copying", || {
            // A retry starts over.
            let undo = std::mem::take(&mut counted);
            ctx.progress(|p| p.bytes_done -= undo);
            let part = part_name(target);
            let _ = fs::remove_file(&part);
            let r = copy_file(src, &part, md, sync, &mut |n| {
                counted += n;
                ctx.progress(|p| p.bytes_done += n);
                !ctx.cancelled()
            })
            .and_then(|()| {
                clear(target)?;
                fs::rename(&part, target)
            });
            if r.is_err() {
                let _ = fs::remove_file(&part);
            }
            r
        })
        .is_some()
    }
}

/// The hidden name a copy is written under until it's complete.
fn part_name(target: &Path) -> PathBuf {
    let mut n = OsString::from(".");
    n.push(target.file_name().unwrap_or_default());
    n.push(".fractale-part");
    target.with_file_name(n)
}

/// Copy a file's bytes, permissions and times to `to` (created new).
/// `step` gets each chunk's size and returns false to cancel.
fn copy_file(from: &Path, to: &Path, md: &Metadata, sync: bool, step: &mut dyn FnMut(u64) -> bool) -> io::Result<()> {
    let src = File::open(from)?;
    let dst = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(to)?;
    let copied = copy_data(&src, &dst, step)?;
    let now = src.metadata()?;
    if copied != now.len() || now.mtime() != md.mtime() || now.len() != md.len() {
        return Err(io::Error::other("the file changed while it was being copied"));
    }
    // From the file as it is now (a retry may follow a chmod).
    dst.set_permissions(now.permissions())?;
    let mut times = fs::FileTimes::new();
    if let Ok(m) = now.modified() {
        times = times.set_modified(m);
    }
    if let Ok(a) = now.accessed() {
        times = times.set_accessed(a);
    }
    dst.set_times(times)?;
    if sync {
        dst.sync_all()?;
    }
    if dst.metadata()?.len() != copied {
        return Err(io::Error::other("the copy is incomplete"));
    }
    Ok(())
}

/// In-kernel copy where available (no round trip through user space, and
/// reflinks on filesystems that do them), otherwise read and write.
fn copy_data(src: &File, dst: &File, step: &mut dyn FnMut(u64) -> bool) -> io::Result<u64> {
    let mut total = 0u64;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        const CHUNK: usize = 8 << 20;
        loop {
            // SAFETY: valid open descriptors; null offsets use and advance
            // the files' own positions.
            let n = unsafe { libc::copy_file_range(src.as_raw_fd(), std::ptr::null_mut(), dst.as_raw_fd(), std::ptr::null_mut(), CHUNK, 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    // Not possible between these files: copy by hand,
                    // from where it stopped (nothing yet in practice).
                    Some(libc::EXDEV | libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL) if total == 0 => break,
                    Some(libc::EINTR) => continue,
                    _ => return Err(e),
                }
            }
            if n == 0 {
                return Ok(total);
            }
            total += n as u64;
            if !step(n as u64) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
    }
    use std::io::{Read, Write};
    let mut buf = vec![0u8; 1 << 20];
    let (mut src, mut dst) = (src, dst);
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        dst.write_all(&buf[..n])?;
        total += n as u64;
        if !step(n as u64) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
    }
}

/// A copied folder gets its permissions and times last (adding files
/// changed its mtime; a read-only folder couldn't have been filled).
fn finish_dir(target: &Path, md: &Metadata) {
    if let Ok(f) = File::open(target) {
        let mut times = fs::FileTimes::new();
        if let Ok(m) = md.modified() {
            times = times.set_modified(m);
        }
        let _ = f.set_times(times);
    }
    let _ = fs::set_permissions(target, md.permissions());
}

fn trash(ctx: &Ctx, spec: &Spec) -> Outcome {
    let mut out = Outcome::default();
    ctx.progress(|p| {
        p.items_total = spec.sources.len() as u64;
        p.counting = false;
    });
    #[allow(unused_mut)]
    let mut tc = trash::TrashContext::default();
    // Finder (the default) needs Apple Events permission and makes a
    // sound; the file manager API does neither.
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        tc.set_delete_method(DeleteMethod::NsFileManager);
    }
    for s in &spec.sources {
        if ctx.cancelled() {
            break;
        }
        ctx.progress(|p| p.current = s.clone());
        let r = ctx.attempt(s, "Moving to the trash", || tc.delete(s).map_err(|e| io::Error::other(trash_error(e))));
        match r {
            Some(()) => out.done += 1,
            None if !ctx.cancelled() => out.skipped += 1,
            None => {}
        }
        ctx.progress(|p| p.items_done += 1);
    }
    out
}

fn trash_error(e: trash::Error) -> String {
    match e {
        #[cfg(target_os = "linux")]
        trash::Error::FileSystem { path, source } => format!("{}: {source}", path.display()),
        e => e.to_string(),
    }
}

fn delete(ctx: &Ctx, spec: &Spec) -> Outcome {
    let mut out = Outcome::default();
    let items: u64 = spec.sources.iter().map(|s| measure(ctx, s).0).sum();
    ctx.progress(|p| {
        p.items_total = items;
        p.counting = false;
    });
    for s in &spec.sources {
        if ctx.cancelled() {
            break;
        }
        if remove(ctx, s) {
            out.done += 1;
        } else if !ctx.cancelled() {
            out.skipped += 1;
        }
    }
    out
}

/// Remove `p` and everything in it; false if anything stayed.
fn remove(ctx: &Ctx, p: &Path) -> bool {
    ctx.progress(|pr| pr.current = p.to_path_buf());
    let Some(md) = ctx.attempt(p, "Reading", || fs::symlink_metadata(p)) else { return false };
    let mut all = true;
    if md.is_dir() {
        if let Some(rd) = ctx.attempt(p, "Reading the folder", || fs::read_dir(p)) {
            for e in rd.flatten() {
                if ctx.cancelled() {
                    return false;
                }
                all &= remove(ctx, &e.path());
            }
        }
        if !all {
            return false;
        }
    }
    let r = ctx.attempt(p, "Deleting", || if md.is_dir() { fs::remove_dir(p) } else { fs::remove_file(p) });
    ctx.progress(|pr| pr.items_done += 1);
    r.is_some()
}

pub fn quick(q: Quick) -> QuickDone {
    let failed = |what: &str, e: io::Error| QuickDone::Failed { what: what.into(), error: e.to_string() };
    match q {
        Quick::Rename { path, name } => {
            let bad = name.is_empty() || name == "." || name == ".." || name.to_string_lossy().contains('/');
            if bad {
                return QuickDone::Failed { what: "Renaming".into(), error: format!("“{}” isn't a valid name", name.to_string_lossy()) };
            }
            let to = path.with_file_name(&name);
            // Same name in another case on a case-insensitive disk is the
            // same file: that rename is fine.
            if let (Ok(a), Ok(b)) = (fs::symlink_metadata(&path), fs::symlink_metadata(&to)) {
                if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                    return QuickDone::Failed { what: "Renaming".into(), error: format!("“{}” already exists", name.to_string_lossy()) };
                }
            }
            match fs::rename(&path, &to) {
                Ok(()) => QuickDone::Renamed { from: path, to },
                Err(e) => failed("Renaming", e),
            }
        }
        Quick::NewFolder { parent } => {
            let mut p = parent.join("New folder");
            if fs::symlink_metadata(&p).is_ok() {
                p = free_name(&parent, std::ffi::OsStr::new("New folder"));
            }
            match fs::create_dir(&p) {
                Ok(()) => QuickDone::Created(p),
                Err(e) => failed("Making a folder", e),
            }
        }
        Quick::Count { paths } => {
            let (mut items, mut bytes) = (0, 0);
            fn walk(p: &Path, items: &mut u64, bytes: &mut u64) {
                let Ok(md) = fs::symlink_metadata(p) else { return };
                *items += 1;
                *bytes += md.blocks() * 512;
                if md.is_dir() {
                    for e in fs::read_dir(p).into_iter().flatten().flatten() {
                        walk(&e.path(), items, bytes);
                    }
                }
            }
            for p in &paths {
                walk(p, &mut items, &mut bytes);
            }
            QuickDone::Counted { paths, items, bytes }
        }
    }
}
