//! What the panes show: which entries, in which order, and what's selected.
//! Plain data and logic, unit-tested; the UI only renders it.

use super::entry::{Entry, Kind};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Date,
    Type,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Date => "date",
            SortBy::Type => "type",
        }
    }

    /// The direction a column sorts in first: names A→Z, sizes largest
    /// first, dates newest first.
    fn natural(self, a: Ordering) -> Ordering {
        match self {
            SortBy::Name | SortBy::Type => a,
            SortBy::Size | SortBy::Date => a.reverse(),
        }
    }
}

/// A pane's sort. Clicking a header cycles natural → reversed → unsorted,
/// like Sentinelle's Applications list; unsorted is the folder's own order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub by: Option<SortBy>,
    pub reversed: bool,
}

impl Sort {
    pub const fn new(by: SortBy) -> Sort {
        Sort { by: Some(by), reversed: false }
    }

    pub fn click(&mut self, by: SortBy) {
        *self = match (self.by, self.reversed) {
            (Some(b), false) if b == by => Sort { by: Some(by), reversed: true },
            (Some(b), true) if b == by => Sort { by: None, reversed: false },
            _ => Sort::new(by),
        };
    }

    /// Order of two entries; `size` gives a folder's total when known.
    pub fn compare(&self, a: &Entry, b: &Entry, size: &dyn Fn(&Entry) -> Option<u64>) -> Ordering {
        let Some(by) = self.by else { return Ordering::Equal };
        let o = match by {
            SortBy::Name => a.key.cmp(&b.key),
            // Unknown sizes go last, in name order, so rows don't jump about
            // while sizes arrive.
            SortBy::Size => match (size(a), size(b)) {
                (Some(x), Some(y)) => SortBy::Size.natural(x.cmp(&y)).then_with(|| a.key.cmp(&b.key)),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => return a.key.cmp(&b.key),
            },
            SortBy::Date => SortBy::Date.natural(a.mtime.cmp(&b.mtime)).then_with(|| a.key.cmp(&b.key)),
            SortBy::Type => type_key(a).cmp(&type_key(b)).then_with(|| a.key.cmp(&b.key)),
        };
        // Size and date already put their natural direction in.
        let o = if matches!(by, SortBy::Size | SortBy::Date) { o } else { by.natural(o) };
        if self.reversed {
            o.reverse()
        } else {
            o
        }
    }
}

fn type_key(e: &Entry) -> (super::entry::Category, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    let ext = e.extension().map(|x| x.as_bytes().to_ascii_lowercase()).unwrap_or_default();
    (e.category, ext)
}

/// Which pane something lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pane {
    Folders,
    Files,
}

impl Pane {
    pub fn of(e: &Entry) -> Pane {
        if e.kind == Kind::Dir {
            Pane::Folders
        } else {
            Pane::Files
        }
    }

    pub fn other(self) -> Pane {
        match self {
            Pane::Folders => Pane::Files,
            Pane::Files => Pane::Folders,
        }
    }
}

/// The selection: a set of names in the open folder (both panes), a cursor
/// (the focused item, in one pane) and, per pane, the anchor of shift-ranges.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub items: HashSet<OsString>,
    pub cursor: Option<(Pane, OsString)>,
    anchor: Option<(Pane, OsString)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// Ctrl (Cmd on macOS): toggle.
    pub toggle: bool,
    /// Shift: extend a range from the anchor.
    pub range: bool,
}

impl Selection {
    pub fn is_selected(&self, name: &OsStr) -> bool {
        self.items.contains(name)
    }

    pub fn is_cursor(&self, pane: Pane, name: &OsStr) -> bool {
        self.cursor.as_ref().is_some_and(|(p, n)| *p == pane && n == name)
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// Select only `name` (and make it the cursor).
    pub fn only(&mut self, pane: Pane, name: &OsStr) {
        self.items.clear();
        self.items.insert(name.to_owned());
        self.cursor = Some((pane, name.to_owned()));
        self.anchor = self.cursor.clone();
    }

    /// A click on `name` in `pane`, whose rows are `order`.
    pub fn click(&mut self, pane: Pane, name: &OsStr, order: &[&OsStr], mods: Mods) {
        if mods.range {
            let anchor = match &self.anchor {
                Some((p, a)) if *p == pane => a.clone(),
                _ => name.to_owned(),
            };
            let (Some(i), Some(j)) = (order.iter().position(|n| *n == anchor), order.iter().position(|n| *n == name)) else {
                return self.only(pane, name);
            };
            if !mods.toggle {
                self.items.clear();
            }
            for n in &order[i.min(j)..=i.max(j)] {
                self.items.insert((*n).to_owned());
            }
            self.cursor = Some((pane, name.to_owned()));
            self.anchor = Some((pane, anchor));
        } else if mods.toggle {
            if !self.items.remove(name) {
                self.items.insert(name.to_owned());
            }
            self.cursor = Some((pane, name.to_owned()));
            self.anchor = self.cursor.clone();
        } else {
            self.only(pane, name);
        }
    }

    /// Move the cursor `delta` rows (or to an end with `i64::MIN`/`MAX`) in
    /// `pane`. With `extend`, the range from the anchor follows; otherwise
    /// the cursor's item alone is selected.
    pub fn step(&mut self, pane: Pane, order: &[&OsStr], delta: i64, extend: bool) {
        if order.is_empty() {
            return;
        }
        let cur = self.cursor.as_ref().filter(|(p, _)| *p == pane).and_then(|(_, n)| order.iter().position(|o| *o == n));
        let next = match cur {
            None => {
                if delta < 0 {
                    order.len() - 1
                } else {
                    0
                }
            }
            Some(i) => (i as i64).saturating_add(delta).clamp(0, order.len() as i64 - 1) as usize,
        };
        let name = order[next];
        if extend {
            self.click(pane, name, order, Mods { toggle: false, range: true });
        } else {
            self.only(pane, name);
        }
    }

    pub fn select_all(&mut self, folders: &[&OsStr], files: &[&OsStr]) {
        self.items = folders.iter().chain(files).map(|n| (*n).to_owned()).collect();
    }

    /// Forget names that are gone after a refresh; keep the rest.
    pub fn retain(&mut self, exists: impl Fn(&OsStr) -> bool) {
        self.items.retain(|n| exists(n));
        if self.cursor.as_ref().is_some_and(|(_, n)| !exists(n)) {
            self.cursor = None;
        }
        if self.anchor.as_ref().is_some_and(|(_, n)| !exists(n)) {
            self.anchor = None;
        }
    }
}

/// Back/forward. Each step remembers which item had the cursor, so going
/// back lands where you were.
#[derive(Clone, Debug, Default)]
pub struct History {
    back: Vec<(PathBuf, Option<OsString>)>,
    forward: Vec<(PathBuf, Option<OsString>)>,
}

impl History {
    /// Leaving `from` (with `cursor`) for somewhere new.
    pub fn push(&mut self, from: PathBuf, cursor: Option<OsString>) {
        self.back.push((from, cursor));
        self.forward.clear();
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn back(&mut self, current: PathBuf, cursor: Option<OsString>) -> Option<(PathBuf, Option<OsString>)> {
        let to = self.back.pop()?;
        self.forward.push((current, cursor));
        Some(to)
    }

    pub fn forward(&mut self, current: PathBuf, cursor: Option<OsString>) -> Option<(PathBuf, Option<OsString>)> {
        let to = self.forward.pop()?;
        self.back.push((current, cursor));
        Some(to)
    }
}

/// Typing letters jumps to the first matching name; letters typed within
/// [`TypeAhead::TIMEOUT`] of each other build one prefix.
#[derive(Default)]
pub struct TypeAhead {
    prefix: String,
    last: Option<std::time::Instant>,
}

impl TypeAhead {
    pub const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(900);

    pub fn push(&mut self, text: &str, now: std::time::Instant) -> &str {
        if self.last.is_none_or(|t| now.duration_since(t) > Self::TIMEOUT) {
            self.prefix.clear();
        }
        self.prefix.push_str(&text.to_lowercase());
        self.last = Some(now);
        &self.prefix
    }

    /// Index of the first name starting with `prefix`, case-insensitively.
    pub fn find(prefix: &str, order: &[&OsStr]) -> Option<usize> {
        order.iter().position(|n| n.to_string_lossy().to_lowercase().starts_with(prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&'static str]) -> Vec<&'static OsStr> {
        v.iter().map(|s| OsStr::new(*s)).collect()
    }

    #[test]
    fn header_cycles() {
        let mut s = Sort::new(SortBy::Size);
        s.click(SortBy::Size);
        assert_eq!(s, Sort { by: Some(SortBy::Size), reversed: true });
        s.click(SortBy::Size);
        assert_eq!(s.by, None);
        s.click(SortBy::Size);
        assert_eq!(s, Sort::new(SortBy::Size));
        s.click(SortBy::Name);
        assert_eq!(s, Sort::new(SortBy::Name));
    }

    #[test]
    fn clicks_and_ranges() {
        let order = names(&["a", "b", "c", "d", "e"]);
        let mut s = Selection::default();
        s.click(Pane::Files, OsStr::new("b"), &order, Mods::default());
        s.click(Pane::Files, OsStr::new("d"), &order, Mods { range: true, toggle: false });
        let mut got: Vec<_> = s.items.iter().cloned().collect();
        got.sort();
        assert_eq!(got, ["b", "c", "d"]);
        s.click(Pane::Files, OsStr::new("c"), &order, Mods { toggle: true, range: false });
        assert!(!s.is_selected(OsStr::new("c")));
        assert!(s.is_cursor(Pane::Files, OsStr::new("c")));
        // Range from the new anchor (c), keeping the rest with toggle.
        s.click(Pane::Files, OsStr::new("e"), &order, Mods { toggle: true, range: true });
        assert_eq!(s.items.len(), 4);
    }

    #[test]
    fn keyboard_steps() {
        let order = names(&["a", "b", "c"]);
        let mut s = Selection::default();
        s.step(Pane::Files, &order, 1, false);
        assert!(s.is_cursor(Pane::Files, OsStr::new("a")), "first step lands on the first item");
        s.step(Pane::Files, &order, 1, true);
        assert_eq!(s.items.len(), 2);
        s.step(Pane::Files, &order, i64::MAX, false);
        assert!(s.is_cursor(Pane::Files, OsStr::new("c")));
        assert_eq!(s.items.len(), 1);
        s.step(Pane::Files, &order, -10, false);
        assert!(s.is_cursor(Pane::Files, OsStr::new("a")));
        s.retain(|n| n != "a");
        assert!(s.cursor.is_none() && s.items.is_empty());
    }

    #[test]
    fn history_round_trip() {
        let mut h = History::default();
        h.push("/a".into(), Some("x".into()));
        let (to, cur) = h.back("/b".into(), None).unwrap();
        assert_eq!((to.to_str().unwrap(), cur.unwrap().to_str().unwrap()), ("/a", "x"));
        assert!(h.can_forward());
        let (to, _) = h.forward("/a".into(), None).unwrap();
        assert_eq!(to.to_str().unwrap(), "/b");
        h.push("/b".into(), None);
        assert!(!h.can_forward());
    }

    #[test]
    fn type_ahead() {
        let order = names(&["Alpha", "beta", "bravo"]);
        let mut t = TypeAhead::default();
        let now = std::time::Instant::now();
        assert_eq!(TypeAhead::find(t.push("b", now), &order), Some(1));
        assert_eq!(TypeAhead::find(t.push("r", now), &order), Some(2));
        let later = now + TypeAhead::TIMEOUT * 2;
        assert_eq!(TypeAhead::find(t.push("a", later), &order), Some(0));
    }
}
