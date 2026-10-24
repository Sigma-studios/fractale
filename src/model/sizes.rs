//! Folder totals for the left pane, joined to the listing by name. They
//! come from a size source: répertoire when it runs, Fractale's own
//! scanning otherwise (see `fallback`). Both follow répertoire's rules:
//! allocated bytes, hard links once per filesystem, never crossing mounts.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

/// What's known about one subfolder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildSize {
    pub total: u64,
    /// Files and folders anywhere below.
    pub files: u64,
    pub dirs: u64,
    pub mtime: i64,
    /// Couldn't be read: its total is only its own entry.
    pub unreadable: bool,
    /// Another filesystem is mounted here: its own total, not part of the
    /// parent's.
    pub mount: Option<String>,
}

/// The sizes of one folder's contents (the open folder or an expanded
/// row).
#[derive(Clone, Debug, Default)]
pub struct FolderSizes {
    pub path: PathBuf,
    /// The folder's own total (its files, its subfolders, its inode).
    /// While sizing runs, the sum of what's known so far.
    pub total: u64,
    /// Bytes directly in the folder: its files and its own inode.
    pub files_here: u64,
    /// Bytes below this folder that the viewer can't see (répertoire's
    /// `hidden_bytes`). Fractale's own scan can't know this.
    pub hidden: u64,
    pub children: HashMap<OsString, ChildSize>,
    /// Every subfolder has been sized.
    pub complete: bool,
}

impl FolderSizes {
    pub fn new(path: PathBuf) -> FolderSizes {
        FolderSizes { path, ..Default::default() }
    }

    /// Total of the subfolders that count towards this folder (mounts
    /// don't).
    pub fn children_total(&self) -> u64 {
        self.children.values().filter(|c| c.mount.is_none()).map(|c| c.total).sum()
    }
}

/// Where sizes come from, for the status strip.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceState {
    /// Fractale's own scan; `busy` while it works.
    OwnScan { busy: bool },
    /// répertoire answers; its state for the open folder's filesystem.
    Repertoire(RepertoireState),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RepertoireState {
    Live,
    Static,
    Reconciling { progress: f32 },
    Scanning { progress: f32 },
    Stale { progress: f32 },
}

/// A message from a size source.
#[derive(Clone, Debug)]
pub enum SizeMsg {
    /// One subfolder's size (a delta; the others are unchanged).
    Child { folder: PathBuf, name: OsString, size: ChildSize },
    /// Everything about a folder at once (replaces what was known).
    Folder(FolderSizes),
    /// Totals for a folder after its children: `complete` once all are in.
    Done { folder: PathBuf, total: u64, files_here: u64, hidden: u64, complete: bool },
    State(SourceState),
}

/// Apply a message to the folders being shown.
pub fn apply(folders: &mut HashMap<PathBuf, FolderSizes>, msg: &SizeMsg) -> bool {
    match msg {
        SizeMsg::Child { folder, name, size } => {
            let f = folders.entry(folder.clone()).or_insert_with(|| FolderSizes::new(folder.clone()));
            if f.children.get(name) == Some(size) {
                return false;
            }
            f.children.insert(name.clone(), size.clone());
            if !f.complete {
                f.total = f.files_here + f.children_total();
            }
            true
        }
        SizeMsg::Folder(fs) => {
            folders.insert(fs.path.clone(), fs.clone());
            true
        }
        SizeMsg::Done { folder, total, files_here, hidden, complete } => {
            let f = folders.entry(folder.clone()).or_insert_with(|| FolderSizes::new(folder.clone()));
            f.total = *total;
            f.files_here = *files_here;
            f.hidden = *hidden;
            f.complete = *complete;
            true
        }
        SizeMsg::State(_) => false,
    }
}
