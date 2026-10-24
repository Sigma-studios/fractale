//! The open folder and everything about how it's shown. Owned by the UI
//! thread; every disk access happens in the services it talks to, whose
//! results it picks up in [`Explorer::poll`].

use super::entry::Entry;
use super::listing::{Count, Lister, Listing, Msg};
use super::view::{History, Mods, Pane, Selection, Sort, SortBy, TypeAhead};
use std::collections::HashMap;
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
    /// Folder totals when known (filled by the size source, later).
    pub sizes: HashMap<OsString, u64>,
}

impl Explorer {
    pub fn new(folder: PathBuf, select: Option<OsString>, wake: impl Fn() + Send + Sync + 'static) -> Explorer {
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
            lister: Lister::spawn(wake),
            sizes: HashMap::new(),
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
                    self.listing = l;
                    if fresh {
                        self.counts.clear();
                        self.sizes.clear();
                        self.selection.clear();
                    }
                    self.rebuild();
                    changed = true;
                }
                Msg::Counts { generation, counts } if generation == self.generation => {
                    self.counts.extend(counts);
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    fn go(&mut self, folder: PathBuf, cursor: Option<OsString>) {
        self.generation += 1;
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

    pub fn refresh(&mut self) {
        self.lister.refresh();
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
        let sizes = &self.sizes;
        let folder_size = |e: &Entry| sizes.get(&e.name).copied();
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

pub enum Activate {
    Nothing,
    Opened,
    /// A file to hand to its default application.
    File(PathBuf),
}
