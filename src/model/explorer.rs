//! The open folder and everything about how it's shown. Owned by the UI
//! thread; every disk access happens in the services it talks to, whose
//! results it picks up in [`Explorer::poll`].

use super::entry::{Entry, Kind};
use super::fallback::Fallback;
use super::repertoire::{self as rep, Repertoire};
use super::search::{Hit, Query, Searcher};
use super::sizes::{self, FolderSizes, SizeMsg, SourceState};
use super::listing::{Count, Lister, Listing, Msg};
use super::view::{History, Mods, Pane, Selection, Sort, SortBy, TypeAhead};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How the file pane draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Grid,
    List,
}

pub struct Explorer {
    /// The folder asked for (the listing may still be the previous one).
    pub folder: PathBuf,
    generation: u64,
    pub listing: Arc<Listing>,
    /// When the current folder was asked for, to show "loading" only if it
    /// takes a while.
    opened_at: Instant,
    pub counts: HashMap<OsString, Count>,
    pub history: History,
    pub show_hidden: bool,
    pub folder_sort: Sort,
    pub file_sort: Sort,
    pub layout: Layout,
    /// Indices into `listing.entries`, filtered and sorted, per pane.
    pub folders: Vec<usize>,
    pub files: Vec<usize>,
    pub selection: Selection,
    /// Put the cursor on this name once the listing arrives (going up,
    /// back, `--select`).
    pending_cursor: Option<OsString>,
    /// Bumped when the views are rebuilt (the UI scrolls to the cursor etc.).
    pub view_version: u64,
    /// Scroll the cursor into view on the next frame.
    pub reveal_cursor: bool,
    pub type_ahead: TypeAhead,
    lister: Lister,
    sizer: Fallback,
    daemon: Repertoire,
    /// répertoire answers.
    pub daemon_up: bool,
    /// Folders répertoire doesn't index (sized by Fractale itself).
    not_indexed: HashSet<PathBuf>,
    /// Whether we've heard from répertoire yet (don't start our own scan
    /// for a moment at startup, it'd be cancelled right away).
    decided: bool,
    started: Instant,
    /// répertoire's "biggest folders here" for a folder.
    pub top: Option<(PathBuf, Vec<(PathBuf, u64)>)>,
    searcher: Searcher,
    /// A search replacing both panes, while there's a query.
    pub search: Option<SearchView>,
    /// Sizes of the open folder and of expanded rows, by path.
    pub sizes: HashMap<PathBuf, FolderSizes>,
    pub source: SourceState,
    /// Expanded rows (absolute paths), remembered per open folder while the
    /// window is open.
    pub expanded: HashMap<PathBuf, BTreeSet<PathBuf>>,
    /// The pointer is over the folder pane: don't reorder rows under it.
    pub freeze_folders: bool,
    resort_pending: bool,
}

impl Explorer {
    pub fn new(folder: PathBuf, select: Option<OsString>, wake: impl Fn() + Send + Sync + 'static) -> Explorer {
        Explorer::with_sources(folder, select, Sources::default(), wake)
    }

    pub fn with_sources(folder: PathBuf, select: Option<OsString>, sources: Sources, wake: impl Fn() + Send + Sync + 'static) -> Explorer {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let w2 = wake.clone();
        let mut e = Explorer {
            folder: PathBuf::new(),
            generation: 0,
            listing: Arc::new(Listing::default()),
            opened_at: Instant::now(),
            counts: HashMap::new(),
            history: History::default(),
            show_hidden: false,
            folder_sort: Sort::new(SortBy::Size),
            file_sort: Sort::new(SortBy::Name),
            layout: Layout::Grid,
            folders: Vec::new(),
            files: Vec::new(),
            selection: Selection::default(),
            pending_cursor: None,
            view_version: 0,
            reveal_cursor: false,
            type_ahead: TypeAhead::default(),
            lister: Lister::spawn(move || w2()),
            sizer: Fallback::spawn(wake.clone()),
            searcher: Searcher::spawn(sources.socket.clone(), wake.clone()),
            search: None,
            daemon: Repertoire::spawn(sources.socket, sources.retry, wake),
            daemon_up: false,
            not_indexed: HashSet::new(),
            decided: false,
            started: Instant::now(),
            top: None,
            sizes: HashMap::new(),
            source: SourceState::OwnScan { busy: false },
            expanded: HashMap::new(),
            freeze_folders: false,
            resort_pending: false,
        };
        e.go(folder, select);
        e
    }

    /// Whether the listing on screen is for the folder asked for.
    pub fn is_current(&self) -> bool {
        self.listing.generation == self.generation && !self.listing.path.as_os_str().is_empty()
    }

    /// Still waiting for the folder after a noticeable moment.
    pub fn is_loading(&self) -> bool {
        !self.is_current() && self.opened_at.elapsed() > Duration::from_millis(150)
    }

    /// Pick up results from the services. True if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.lister.rx.try_recv() {
            match msg {
                Msg::Listed(l) if l.generation == self.generation => {
                    let fresh = !self.is_current();
                    let before = std::mem::replace(&mut self.listing, l);
                    if fresh {
                        self.counts.clear();
                        self.selection.clear();
                    } else {
                        // Tell the sizer which subfolders changed.
                        let names = changed_folders(&before, &self.listing);
                        let files = files_changed(&before, &self.listing);
                        // répertoire sees changes by itself.
                        if (!names.is_empty() || files) && !self.uses_daemon() {
                            self.sizer.changed(&self.folder, names);
                        }
                    }
                    self.rebuild();
                    if fresh {
                        self.watch_sizes();
                    }
                    changed = true;
                }
                Msg::Counts { generation, counts } if generation == self.generation => {
                    self.counts.extend(counts);
                    changed = true;
                }
                _ => {}
            }
        }
        let mut resort = false;
        let mut rewatch = false;
        while let Ok(msg) = self.daemon.rx.try_recv() {
            match msg {
                rep::Msg::Up => {
                    if !self.daemon_up || !self.decided {
                        self.daemon_up = true;
                        self.decided = true;
                        self.not_indexed.clear();
                        rewatch = true;
                    }
                }
                rep::Msg::Down => {
                    if self.daemon_up || !self.decided {
                        self.daemon_up = false;
                        self.decided = true;
                        rewatch = true;
                    }
                }
                rep::Msg::NotIndexed(p) => {
                    if self.not_indexed.insert(p.clone()) && (p == self.folder || self.wants(&p)) {
                        rewatch = true;
                    }
                }
                rep::Msg::Size(msg) => {
                    if !self.uses_daemon() {
                        continue;
                    }
                    if let SizeMsg::State(s) = &msg {
                        self.source = s.clone();
                        changed = true;
                        continue;
                    }
                    let ours = match &msg {
                        SizeMsg::Child { folder, .. } | SizeMsg::Done { folder, .. } => self.wants(folder),
                        SizeMsg::Folder(f) => self.wants(&f.path),
                        SizeMsg::State(_) => false,
                    };
                    if ours && sizes::apply(&mut self.sizes, &msg) {
                        changed = true;
                        resort = true;
                    }
                }
                rep::Msg::Top { path, folders } => {
                    if path == self.folder {
                        self.top = Some((path, folders));
                        changed = true;
                    }
                }
            }
        }
        while let Ok(r) = self.searcher.rx.try_recv() {
            if let Some(v) = self.search.as_mut().filter(|v| v.generation == r.generation) {
                v.hits = r.hits;
                v.done = r.done;
                v.partial = r.partial;
                v.indexed = r.indexed;
                v.cursor = v.cursor.min(v.hits.len().saturating_sub(1));
                changed = true;
            }
        }
        if !self.decided && self.started.elapsed() > Duration::from_millis(300) {
            self.decided = true;
            rewatch = true;
        }
        if rewatch {
            self.watch_sizes();
            changed = true;
        }
        while let Ok(msg) = self.sizer.rx.try_recv() {
            if self.uses_daemon() {
                continue;
            }
            if let SizeMsg::State(s) = &msg {
                self.source = s.clone();
                continue;
            }
            let ours = match &msg {
                SizeMsg::Child { folder, .. } | SizeMsg::Done { folder, .. } => self.wants(folder),
                SizeMsg::Folder(f) => self.wants(&f.path),
                SizeMsg::State(_) => false,
            };
            if ours && sizes::apply(&mut self.sizes, &msg) {
                changed = true;
                resort = true;
            }
        }
        if (resort || self.resort_pending) && self.folder_sort.by == Some(SortBy::Size) {
            if self.freeze_folders {
                self.resort_pending = true;
            } else {
                self.resort_pending = false;
                self.rebuild();
            }
        }
        changed
    }

    /// Sizes for this path matter now (the open folder or an expanded row).
    fn wants(&self, path: &Path) -> bool {
        path == self.folder || self.expanded.get(&self.folder).is_some_and(|e| e.contains(path))
    }

    /// The open folder's sizes come from répertoire.
    pub fn uses_daemon(&self) -> bool {
        self.daemon_up && !self.not_indexed.contains(&self.folder)
    }

    /// Ask the size source for the open folder and its expanded rows, the
    /// rows on screen first. What's shown stays until the new answers come,
    /// so switching sources has no gap.
    pub fn watch_sizes(&mut self) {
        if !self.decided || !self.is_current() && self.listing.path != self.folder {
            return;
        }
        let order: Vec<OsString> = self.order(Pane::Folders).into_iter().map(OsStr::to_owned).collect();
        let expanded: Vec<PathBuf> = self.expanded.get(&self.folder).map(|e| e.iter().cloned().collect()).unwrap_or_default();
        self.sizes.retain(|p, _| *p == self.folder || expanded.contains(p));
        if self.uses_daemon() {
            self.sizer.idle();
            self.daemon.watch(&self.folder, expanded);
        } else {
            self.source = SourceState::OwnScan { busy: true };
            self.sizer.watch(&self.folder, order, expanded);
        }
    }

    /// Search for `text` under the open folder (or everywhere). An empty
    /// text closes the search.
    pub fn set_search(&mut self, text: &str, everywhere: bool) {
        if text.trim().is_empty() {
            self.close_search();
            return;
        }
        let query = Query { text: text.trim().to_string(), under: (!everywhere).then(|| self.folder.clone()), show_hidden: self.show_hidden };
        let generation = self.searcher.search(query, self.daemon_up);
        let cursor = self.search.as_ref().map_or(0, |v| v.cursor);
        self.search = Some(SearchView { text: text.to_string(), everywhere, generation, hits: Vec::new(), done: false, partial: false, indexed: false, cursor });
    }

    pub fn close_search(&mut self) {
        self.searcher.cancel();
        self.search = None;
    }

    /// Open a search result: folders here, files with their app. With
    /// `in_folder`, open the folder containing it with it selected.
    pub fn open_hit(&mut self, hit: &Hit, in_folder: bool) -> Activate {
        self.close_search();
        let opens_as_folder = hit.kind == Kind::Dir;
        if in_folder || !opens_as_folder {
            if let (Some(parent), Some(name)) = (hit.path.parent(), hit.path.file_name()) {
                if in_folder {
                    self.history.push(self.folder.clone(), self.cursor_name());
                    self.go(parent.to_path_buf(), Some(name.to_owned()));
                    return Activate::Opened;
                }
            }
            return Activate::File(hit.path.clone());
        }
        self.open(hit.path.clone());
        Activate::Opened
    }

    /// répertoire's biggest folders anywhere below the open one.
    pub fn request_top(&mut self) {
        self.top = None;
        if self.uses_daemon() {
            self.daemon.top(&self.folder, 20);
        }
    }

    /// Size these rows first (they're on screen).
    pub fn prioritize(&mut self, visible: Vec<OsString>) {
        if self.uses_daemon() || !self.decided {
            return;
        }
        let mut order = visible;
        for n in self.order(Pane::Folders) {
            if !order.iter().any(|o| o == n) {
                order.push(n.to_owned());
            }
        }
        let expanded: Vec<PathBuf> = self.expanded.get(&self.folder).map(|e| e.iter().cloned().collect()).unwrap_or_default();
        self.sizer.watch(&self.folder, order, expanded);
    }

    pub fn folder_sizes(&self) -> Option<&FolderSizes> {
        self.sizes.get(&self.folder)
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.get(&self.folder).is_some_and(|e| e.contains(path))
    }

    /// Expand or collapse a row (a folder anywhere below the open one).
    pub fn toggle_expanded(&mut self, path: PathBuf) {
        let set = self.expanded.entry(self.folder.clone()).or_default();
        if !set.remove(&path) {
            set.insert(path);
        } else {
            // Collapsing a row collapses what was open below it.
            set.retain(|p| !p.starts_with(&path));
        }
        self.watch_sizes();
    }

    fn go(&mut self, folder: PathBuf, cursor: Option<OsString>) {
        self.generation += 1;
        self.top = None;
        self.folder = folder;
        self.opened_at = Instant::now();
        self.pending_cursor = cursor;
        self.lister.open(self.folder.clone(), self.generation);
    }

    fn cursor_name(&self) -> Option<OsString> {
        self.selection.cursor.as_ref().map(|(_, n)| n.clone())
    }

    /// Open a folder (adds to history).
    pub fn open(&mut self, folder: PathBuf) {
        if folder == self.folder {
            return;
        }
        self.history.push(self.folder.clone(), self.cursor_name());
        self.go(folder, None);
    }

    /// Open the parent, with the folder we came from under the cursor.
    pub fn up(&mut self) {
        let Some(parent) = self.folder.parent().map(Path::to_path_buf) else { return };
        let came_from = self.folder.file_name().map(OsStr::to_owned);
        self.history.push(self.folder.clone(), self.cursor_name());
        self.go(parent, came_from);
    }

    pub fn back(&mut self) {
        if let Some((to, cursor)) = self.history.back(self.folder.clone(), self.cursor_name()) {
            self.go(to, cursor);
        }
    }

    pub fn forward(&mut self) {
        if let Some((to, cursor)) = self.history.forward(self.folder.clone(), self.cursor_name()) {
            self.go(to, cursor);
        }
    }

    /// F5: list again and measure again.
    pub fn refresh(&mut self) {
        self.lister.refresh();
        if self.uses_daemon() {
            self.watch_sizes();
        } else {
            self.sizes.clear();
            self.sizer.forget(&self.folder);
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }

    pub fn sort_click(&mut self, pane: Pane, by: SortBy) {
        match pane {
            Pane::Folders => self.folder_sort.click(by),
            Pane::Files => self.file_sort.click(by),
        }
        self.rebuild();
    }

    pub fn entry(&self, i: usize) -> &Entry {
        &self.listing.entries[i]
    }

    pub fn find(&self, name: &OsStr) -> Option<&Entry> {
        self.listing.entries.iter().find(|e| e.name == name)
    }

    /// Names of a pane, in display order.
    pub fn order(&self, pane: Pane) -> Vec<&OsStr> {
        let idx = match pane {
            Pane::Folders => &self.folders,
            Pane::Files => &self.files,
        };
        idx.iter().map(|&i| self.listing.entries[i].name.as_os_str()).collect()
    }

    /// Re-filter and re-sort after a listing, sort or filter change, keeping
    /// the selection by name.
    pub fn rebuild(&mut self) {
        let entries = &self.listing.entries;
        let visible = |e: &Entry| self.show_hidden || !e.is_hidden();
        let mut folders: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].is_folder() && visible(&entries[i])).collect();
        let mut files: Vec<usize> = (0..entries.len()).filter(|&i| !entries[i].is_folder() && visible(&entries[i])).collect();
        let sizes = self.sizes.get(&self.folder);
        let folder_size = |e: &Entry| sizes.and_then(|s| s.children.get(&e.name)).map(|c| c.total);
        let file_size = |e: &Entry| Some(e.bytes);
        let (fs, ls) = (self.folder_sort, self.file_sort);
        if fs.by.is_some() {
            folders.sort_by(|&a, &b| fs.compare(&entries[a], &entries[b], &folder_size));
        }
        if ls.by.is_some() {
            files.sort_by(|&a, &b| ls.compare(&entries[a], &entries[b], &file_size));
        }
        self.folders = folders;
        self.files = files;
        let names: std::collections::HashSet<&OsStr> = self.folders.iter().chain(&self.files).map(|&i| entries[i].name.as_os_str()).collect();
        self.selection.retain(|n| names.contains(n));
        if let Some(name) = self.pending_cursor.take() {
            if let Some(e) = self.find(&name) {
                let pane = Pane::of(e);
                self.selection.only(pane, &name);
                self.reveal_cursor = true;
            } else if !self.is_current() {
                self.pending_cursor = Some(name);
            }
        }
        self.view_version += 1;
    }

    pub fn click(&mut self, pane: Pane, name: &OsStr, mods: Mods) {
        let order = self.order(pane).into_iter().map(OsStr::to_owned).collect::<Vec<_>>();
        let refs: Vec<&OsStr> = order.iter().map(|n| n.as_os_str()).collect();
        self.selection.click(pane, name, &refs, mods);
    }

    /// Arrow keys, Home/End, Page Up/Down.
    pub fn step(&mut self, pane: Pane, delta: i64, extend: bool) {
        let order: Vec<OsString> = self.order(pane).into_iter().map(OsStr::to_owned).collect();
        let refs: Vec<&OsStr> = order.iter().map(|n| n.as_os_str()).collect();
        self.selection.step(pane, &refs, delta, extend);
        self.reveal_cursor = true;
    }

    pub fn select_all(&mut self) {
        let f: Vec<OsString> = self.order(Pane::Folders).into_iter().map(OsStr::to_owned).collect();
        let g: Vec<OsString> = self.order(Pane::Files).into_iter().map(OsStr::to_owned).collect();
        let fr: Vec<&OsStr> = f.iter().map(|n| n.as_os_str()).collect();
        let gr: Vec<&OsStr> = g.iter().map(|n| n.as_os_str()).collect();
        self.selection.select_all(&fr, &gr);
    }

    /// Letters typed with no text field focused: jump to the first name
    /// starting with them, in `pane`.
    pub fn type_ahead(&mut self, pane: Pane, text: &str) {
        let prefix = self.type_ahead.push(text, Instant::now()).to_string();
        let order: Vec<OsString> = self.order(pane).into_iter().map(OsStr::to_owned).collect();
        let refs: Vec<&OsStr> = order.iter().map(|n| n.as_os_str()).collect();
        if let Some(i) = TypeAhead::find(&prefix, &refs) {
            self.selection.only(pane, refs[i]);
            self.reveal_cursor = true;
        }
    }

    /// What opening the cursor's item does.
    pub fn activate(&mut self, pane: Pane, name: &OsStr) -> Activate {
        let Some(e) = self.find(name) else { return Activate::Nothing };
        let path = self.folder.join(name);
        let _ = pane;
        if e.opens_as_folder() {
            self.open(path);
            Activate::Opened
        } else {
            Activate::File(path)
        }
    }

    /// The selected entries' paths, in display order (folders first).
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.folders
            .iter()
            .chain(&self.files)
            .map(|&i| &self.listing.entries[i])
            .filter(|e| self.selection.is_selected(&e.name))
            .map(|e| self.folder.join(&e.name))
            .collect()
    }
}

/// Subfolders that appeared, disappeared or changed between two listings
/// of the same folder (a folder's mtime moves when its entries change).
fn changed_folders(before: &Listing, after: &Listing) -> Vec<OsString> {
    let old: HashMap<&OsStr, (i64, u64)> = before.entries.iter().filter(|e| e.kind == Kind::Dir).map(|e| (e.name.as_os_str(), (e.mtime, e.ino))).collect();
    let new: HashMap<&OsStr, (i64, u64)> = after.entries.iter().filter(|e| e.kind == Kind::Dir).map(|e| (e.name.as_os_str(), (e.mtime, e.ino))).collect();
    let mut out: Vec<OsString> = new.iter().filter(|(n, v)| old.get(*n) != Some(*v)).map(|(n, _)| n.to_os_string()).collect();
    out.extend(old.keys().filter(|n| !new.contains_key(*n)).map(|n| n.to_os_string()));
    out
}

/// Files directly in the folder changed (size, mtime, added, removed).
fn files_changed(before: &Listing, after: &Listing) -> bool {
    let sig = |l: &Listing| -> Vec<(OsString, u64, i64)> {
        let mut v: Vec<_> = l.entries.iter().filter(|e| e.kind != Kind::Dir).map(|e| (e.name.clone(), e.bytes, e.mtime)).collect();
        v.sort();
        v
    };
    sig(before) != sig(after)
}

/// Search results shown instead of the panes.
#[derive(Clone, Debug)]
pub struct SearchView {
    pub text: String,
    pub everywhere: bool,
    pub generation: u64,
    pub hits: Vec<Hit>,
    pub done: bool,
    pub partial: bool,
    /// From répertoire's index (otherwise a walk of the folder).
    pub indexed: bool,
    pub cursor: usize,
}

/// Where sizes come from.
pub struct Sources {
    pub socket: PathBuf,
    /// How often to look for répertoire while it isn't there.
    pub retry: Duration,
    /// The thumbnail cache (the shared one; tests use their own).
    pub thumbnails: PathBuf,
}

impl Default for Sources {
    fn default() -> Self {
        Sources { socket: repertoire_proto::socket_path(), retry: Duration::from_secs(60), thumbnails: super::thumbs::cache::root() }
    }
}

pub enum Activate {
    Nothing,
    Opened,
    /// A file to hand to its default application.
    File(PathBuf),
}
