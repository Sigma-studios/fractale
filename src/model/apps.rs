//! Which application opens a file.
//!
//! Fractale answers "what is this file, and what opens it" the way the rest
//! of a freedesktop desktop (KDE Plasma in particular) does, so that Enter in
//! Fractale and a double click in Dolphin agree. Three specifications are
//! involved:
//!
//! - **shared-mime-info**: [`MimeDb`] merges `globs2`, `magic`, `aliases` and
//!   `subclasses` from every `mime/` directory under the XDG data dirs and
//!   guesses a file's type from its name, then from its first bytes.
//! - **desktop entries**: [`Apps`] reads `applications/**/*.desktop` and
//!   turns `Exec` lines into argv lists ([`App::commands`]).
//! - **mime-apps**: [`Apps`] reads every `mimeapps.list`, including the
//!   desktop-specific `kde-mimeapps.list`, to pick the default application
//!   and to order the "Open with" list.
//!
//! Everything is read once into memory; it is meant to be built on a worker
//! thread at startup and then shared (`MimeDb` and `Apps` are `Send + Sync`).
//! After that the only disk access is [`MimeDb::type_of`] (one `stat`, plus a
//! read of at most [`SNIFF_LEN`] bytes when the name is not conclusive) and
//! [`is_executable`].
//!
//! Nothing here is Linux-only at compile time: on macOS the XDG directories
//! do not exist, so every database simply comes out empty.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Upper bound on how much of a file is read for content sniffing. Magic
/// rules almost all look at the first few hundred bytes; GLib uses the same
/// 4 KiB cap, and reading one page costs the same as reading less.
pub const SNIFF_LEN: usize = 4096;

const OCTET_STREAM: &str = "application/octet-stream";
const TEXT_PLAIN: &str = "text/plain";
const ZERO_SIZE: &str = "application/x-zerosize";
const DIRECTORY: &str = "inode/directory";

// ---------------------------------------------------------------------------
// XDG base directories
// ---------------------------------------------------------------------------

fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// `$XDG_DATA_HOME` followed by `$XDG_DATA_DIRS`, in precedence order, with
/// the basedir spec's defaults (`~/.local/share`, `/usr/local/share:/usr/share`).
pub fn xdg_data_dirs() -> Vec<PathBuf> {
    xdg_dirs(
        "XDG_DATA_HOME",
        ".local/share",
        "XDG_DATA_DIRS",
        "/usr/local/share:/usr/share",
    )
}

/// `$XDG_CONFIG_HOME` followed by `$XDG_CONFIG_DIRS`, in precedence order,
/// with the basedir spec's defaults (`~/.config`, `/etc/xdg`).
pub fn xdg_config_dirs() -> Vec<PathBuf> {
    xdg_dirs("XDG_CONFIG_HOME", ".config", "XDG_CONFIG_DIRS", "/etc/xdg")
}

fn xdg_dirs(
    home_var: &str,
    home_default: &str,
    list_var: &str,
    list_default: &str,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // The basedir spec says relative paths in these variables are invalid
    // and must be ignored, which also covers the unset/empty case.
    match env_nonempty(home_var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(p) => out.push(p),
        None => {
            if let Some(home) = env_nonempty("HOME") {
                out.push(Path::new(&home).join(home_default));
            }
        }
    }
    let list = env_nonempty(list_var).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = list
        .split(':')
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect();
    if dirs.is_empty() {
        dirs = list_default.split(':').map(PathBuf::from).collect();
    }
    out.extend(dirs);
    dedup_paths(&out)
}

/// NixOS profiles routinely list the same directory twice; reading it twice
/// would only cost time (and break "first directory wins" bookkeeping).
fn dedup_paths(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.iter()
        .filter(|d| seen.insert((*d).clone()))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Shared MIME database
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Glob {
    weight: u32,
    mime: String,
    /// Pattern length in chars: on equal weight the longest pattern wins
    /// (`*.tar.gz` over `*.gz`).
    len: usize,
    case_sensitive: bool,
}

#[derive(Debug, Default)]
struct GlobMatches {
    best: Vec<String>,
    all: Vec<String>,
}

#[derive(Debug, Clone)]
struct PatternGlob {
    glob: Glob,
    /// Lowercased unless the glob is case-sensitive.
    pattern: Vec<char>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Matchlet {
    indent: u32,
    offset: usize,
    range: usize,
    value: Vec<u8>,
    mask: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct MagicSection {
    priority: u32,
    mime: String,
    rules: Vec<Matchlet>,
}

/// The merged shared-mime-info database.
#[derive(Debug, Default, Clone)]
pub struct MimeDb {
    // Globs are split the way the spec suggests: literal names and plain
    // `*.ext` patterns go in hash tables, so the common case is a couple of
    // lookups; only the rest needs fnmatch.
    literal_cs: HashMap<String, Vec<Glob>>,
    literal_ci: HashMap<String, Vec<Glob>>,
    suffix_cs: HashMap<String, Vec<Glob>>,
    suffix_ci: HashMap<String, Vec<Glob>>,
    patterns: Vec<PatternGlob>,
    /// Sorted by decreasing priority; first match wins.
    magic: Vec<MagicSection>,
    aliases: HashMap<String, String>,
    aliases_of: HashMap<String, Vec<String>>,
    parents: HashMap<String, Vec<String>>,
}

impl MimeDb {
    /// Load and merge `<dir>/mime` for each data dir, highest precedence
    /// first (`$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`). Missing or broken
    /// files are skipped: a half-installed package must not take down type
    /// detection for everything else.
    pub fn load(data_dirs: &[PathBuf]) -> MimeDb {
        let mut db = MimeDb::default();
        // Types redefined with __NOGLOBS__/__NOMAGIC__ in a higher directory
        // ignore the rules of every lower one.
        let mut no_globs: HashSet<String> = HashSet::new();
        let mut no_magic: HashSet<String> = HashSet::new();
        // NixOS can have several full copies of the database (one per
        // profile); identical rules are merged instead of matched N times.
        let mut seen_globs: HashSet<(String, String, bool)> = HashSet::new();
        let mut seen_magic: HashSet<(u32, String, Vec<Matchlet>)> = HashSet::new();
        let mut magic = Vec::new();

        for dir in dedup_paths(data_dirs) {
            let mime_dir = dir.join("mime");
            let (globs, dir_no_globs) = read_globs(&mime_dir);
            for (weight, mime, pattern, cs) in globs {
                if no_globs.contains(&mime) {
                    continue;
                }
                if seen_globs.insert((mime.clone(), pattern.clone(), cs)) {
                    db.add_glob(weight, mime, &pattern, cs);
                }
            }
            no_globs.extend(dir_no_globs);

            let (sections, dir_no_magic) = read_magic(&mime_dir.join("magic"));
            for s in sections {
                if no_magic.contains(&s.mime) {
                    continue;
                }
                if seen_magic.insert((s.priority, s.mime.clone(), s.rules.clone())) {
                    magic.push(s);
                }
            }
            no_magic.extend(dir_no_magic);

            for (alias, canonical) in read_pairs(&mime_dir.join("aliases")) {
                db.aliases.entry(alias).or_insert(canonical);
            }
            for (child, parent) in read_pairs(&mime_dir.join("subclasses")) {
                let list = db.parents.entry(child).or_default();
                if !list.contains(&parent) {
                    list.push(parent);
                }
            }
        }
        // Stable: on equal priority the higher-precedence directory stays first.
        magic.sort_by_key(|s| std::cmp::Reverse(s.priority));
        db.magic = magic;
        for (alias, canonical) in &db.aliases {
            db.aliases_of
                .entry(canonical.clone())
                .or_default()
                .push(alias.clone());
        }
        for list in db.aliases_of.values_mut() {
            list.sort();
        }
        db
    }

    /// [`MimeDb::load`] on the XDG data dirs of the environment.
    pub fn from_env() -> MimeDb {
        MimeDb::load(&xdg_data_dirs())
    }

    fn add_glob(&mut self, weight: u32, mime: String, pattern: &str, cs: bool) {
        let glob = Glob {
            weight,
            mime,
            len: pattern.chars().count(),
            case_sensitive: cs,
        };
        let key = |s: &str| if cs { s.to_string() } else { s.to_lowercase() };
        if !has_wildcard(pattern) {
            let table = if cs {
                &mut self.literal_cs
            } else {
                &mut self.literal_ci
            };
            table.entry(key(pattern)).or_default().push(glob);
        } else if let Some(ext) = pattern
            .strip_prefix("*.")
            .filter(|e| !e.is_empty() && !has_wildcard(e))
        {
            let table = if cs {
                &mut self.suffix_cs
            } else {
                &mut self.suffix_ci
            };
            table.entry(key(ext)).or_default().push(glob);
        } else {
            self.patterns.push(PatternGlob {
                glob,
                pattern: key(pattern).chars().collect(),
            });
        }
    }

    /// The MIME type of `path`, following symlinks.
    ///
    /// Follows the spec's recommended order: the name first, and the
    /// content (at most [`SNIFF_LEN`] bytes) only when the name matches
    /// nothing or more than one type. Directories are `inode/directory`,
    /// other non-regular files get their `inode/*` type without being
    /// opened (opening a FIFO would block), and files that cannot be read
    /// fall back to the name, then `application/octet-stream`.
    ///
    /// One refinement over the spec, shared by KDE (QMimeDatabase) and GLib:
    /// "more than one type" counts lower-weight globs too, and the content
    /// may confirm one of them. `x.py` is `text/x-python` (weight 60) by
    /// name, but `text/x-python3` (weight 50, also `*.py`) when it starts
    /// with a python3 shebang.
    pub fn type_of(&self, path: &Path) -> String {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let globs = || {
            name.as_deref()
                .map(|n| self.glob_matches(n))
                .unwrap_or_default()
        };
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return first_or_octet(globs().best),
        };
        if meta.is_dir() {
            return DIRECTORY.to_string();
        }
        if let Some(t) = special_file_type(&meta) {
            return t.to_string();
        }
        let globs = globs();
        if globs.all.len() == 1 {
            return first_or_octet(globs.best);
        }
        match read_head(path, SNIFF_LEN) {
            Ok(data) => self.resolve(globs, &data),
            Err(_) => first_or_octet(globs.best),
        }
    }

    /// The MIME type the file name alone suggests, without touching the
    /// disk. When several types tie, this is the first (the one a sniff
    /// would fall back to); `None` when no glob matches.
    pub fn type_of_name(&self, name: &str) -> Option<String> {
        self.glob_matches(name).best.into_iter().next()
    }

    /// Whether `mime` is `parent` or a subclass of it (after resolving
    /// aliases), including the implicit rules: every `text/*` is a
    /// `text/plain` and every streamable type is an `application/octet-stream`.
    pub fn is_a(&self, mime: &str, parent: &str) -> bool {
        let parent = self.unalias(parent);
        self.ancestors(mime).iter().any(|a| a == parent)
    }

    /// The canonical name for `mime` (itself when it is not an alias).
    pub fn unalias<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map(String::as_str).unwrap_or(mime)
    }

    /// `mime` (unaliased) followed by all its ancestors, nearest first.
    /// `application/octet-stream` always comes last, because it is
    /// everyone's parent and the least useful fallback.
    pub fn ancestors(&self, mime: &str) -> Vec<String> {
        let start = self.unalias(mime).to_string();
        let mut out = vec![start.clone()];
        let mut i = 0;
        while i < out.len() {
            for p in self.direct_parents(&out[i]) {
                if p != OCTET_STREAM && !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        if is_streamable(&start) && start != OCTET_STREAM {
            out.push(OCTET_STREAM.to_string());
        }
        out
    }

    fn direct_parents(&self, mime: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .parents
            .get(mime)
            .map(|ps| ps.iter().map(|p| self.unalias(p).to_string()).collect())
            .unwrap_or_default();
        if mime.starts_with("text/") && mime != TEXT_PLAIN && !out.iter().any(|p| p == TEXT_PLAIN) {
            out.push(TEXT_PLAIN.to_string());
        }
        out
    }

    /// The canonical name plus every alias of it: desktop files and
    /// mimeapps.list may use either.
    fn names_of<'a>(&'a self, canonical: &'a str) -> Vec<&'a str> {
        let mut names = vec![canonical];
        if let Some(aliases) = self.aliases_of.get(canonical) {
            names.extend(aliases.iter().map(String::as_str));
        }
        names
    }

    /// The glob matches for `name`: `best` holds the distinct types with
    /// the highest weight and then the longest pattern, `all` every matching
    /// type, best first.
    fn glob_matches(&self, name: &str) -> GlobMatches {
        let mut hits = self.glob_hits(name);
        // Stable, so equal globs keep database (directory precedence) order.
        hits.sort_by(|a, b| b.weight.cmp(&a.weight).then(b.len.cmp(&a.len)));
        let mut m = GlobMatches::default();
        let Some(top) = hits.first().map(|g| (g.weight, g.len)) else {
            return m;
        };
        for g in &hits {
            if (g.weight, g.len) == top && !m.best.contains(&g.mime) {
                m.best.push(g.mime.clone());
            }
            if !m.all.contains(&g.mime) {
                m.all.push(g.mime.clone());
            }
        }
        m
    }

    fn glob_hits(&self, name: &str) -> Vec<&Glob> {
        let lower = name.to_lowercase();
        // Tier 1: literal names ("Makefile") beat every wildcard pattern.
        let mut hits: Vec<&Glob> = Vec::new();
        hits.extend(self.literal_cs.get(name).into_iter().flatten());
        hits.extend(self.literal_ci.get(&lower).into_iter().flatten());
        if !hits.is_empty() {
            return prefer_case_sensitive(hits);
        }
        // Tier 2: "*.ext". Trying the text after every dot lets "a.tar.gz"
        // see both "tar.gz" and "gz"; the length rule then picks tar.gz.
        for (i, _) in name.match_indices('.') {
            hits.extend(self.suffix_cs.get(&name[i + 1..]).into_iter().flatten());
        }
        for (i, _) in lower.match_indices('.') {
            hits.extend(self.suffix_ci.get(&lower[i + 1..]).into_iter().flatten());
        }
        if !hits.is_empty() {
            return prefer_case_sensitive(hits);
        }
        // Tier 3: everything else, through fnmatch.
        let exact: Vec<char> = name.chars().collect();
        let folded: Vec<char> = lower.chars().collect();
        for p in &self.patterns {
            let subject = if p.glob.case_sensitive {
                &exact
            } else {
                &folded
            };
            if fnmatch(&p.pattern, subject) {
                hits.push(&p.glob);
            }
        }
        prefer_case_sensitive(hits)
    }

    /// Glob ambiguity (or no glob at all) settled by the content, as in the
    /// spec's "recommended checking order" (and QMimeDatabase).
    fn resolve(&self, globs: GlobMatches, data: &[u8]) -> String {
        // Binary data without a magic match says nothing about which glob
        // is right, so it does not take part in the tie-break.
        let sniffed = if data.is_empty() {
            Some(ZERO_SIZE)
        } else if let Some(m) = self.match_magic(data) {
            Some(m)
        } else if looks_like_text(data) {
            Some(TEXT_PLAIN)
        } else {
            None
        };
        if globs.all.is_empty() {
            return sniffed.unwrap_or(OCTET_STREAM).to_string();
        }
        if let Some(sniffed) = sniffed {
            if globs.best.iter().any(|g| g == sniffed) {
                return sniffed.to_string();
            }
            // A glob candidate that the content confirms (equal or
            // subclass) wins: text in a *.ts file is Qt Linguist rather
            // than an MPEG transport stream.
            if let Some(g) = globs.all.iter().find(|g| self.is_a(g, sniffed)) {
                return g.clone();
            }
        }
        first_or_octet(globs.best)
    }

    fn match_magic(&self, data: &[u8]) -> Option<&str> {
        self.magic
            .iter()
            .find(|s| section_matches(&s.rules, data))
            .map(|s| s.mime.as_str())
    }
}

fn first_or_octet(globs: Vec<String>) -> String {
    globs
        .into_iter()
        .next()
        .unwrap_or_else(|| OCTET_STREAM.to_string())
}

/// A case-sensitive hit is a deliberate statement about this exact
/// spelling (`*.C` is C++), so it beats a case-insensitive one of the same
/// tier (`*.c`), as in GLib's lookup.
fn prefer_case_sensitive(hits: Vec<&Glob>) -> Vec<&Glob> {
    if hits.iter().any(|g| g.case_sensitive) {
        hits.into_iter().filter(|g| g.case_sensitive).collect()
    } else {
        hits
    }
}

fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// The spec only exempts `inode/*`; scheme handlers and volume content
/// types are not file formats either, so they get no octet-stream parent.
fn is_streamable(mime: &str) -> bool {
    !(mime.starts_with("inode/")
        || mime.starts_with("x-scheme-handler/")
        || mime.starts_with("x-content/"))
}

#[cfg(unix)]
fn special_file_type(meta: &fs::Metadata) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    let ft = meta.file_type();
    if ft.is_char_device() {
        Some("inode/chardevice")
    } else if ft.is_block_device() {
        Some("inode/blockdevice")
    } else if ft.is_fifo() {
        Some("inode/fifo")
    } else if ft.is_socket() {
        Some("inode/socket")
    } else {
        None
    }
}

#[cfg(not(unix))]
fn special_file_type(_meta: &fs::Metadata) -> Option<&'static str> {
    None
}

fn read_head(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut buf = Vec::with_capacity(max);
    file.take(max as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// The spec's text heuristic, on the first 128 bytes as it suggests (and
/// as Qt does): ASCII control characters mean binary, but high-bit bytes do
/// not (they are how UTF-8 looks). A UTF-16 byte order mark means text.
fn looks_like_text(data: &[u8]) -> bool {
    if data.starts_with(b"\xff\xfe") || data.starts_with(b"\xfe\xff") {
        return true;
    }
    data.iter()
        .take(128)
        .all(|&b| b >= 0x20 || matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x1b | 0x08))
}

/// A globs2 line: `(weight, mime, pattern, case_sensitive)`.
type GlobLine = (u32, String, String, bool);

/// globs2 entries plus the types
/// this directory marks `__NOGLOBS__`. Falls back to the old `globs` file
/// (implicit weight 50) when there is no globs2.
fn read_globs(mime_dir: &Path) -> (Vec<GlobLine>, HashSet<String>) {
    let mut out = Vec::new();
    let mut no_globs = HashSet::new();
    let (text, v2) = match fs::read_to_string(mime_dir.join("globs2")) {
        Ok(t) => (t, true),
        Err(_) => match fs::read_to_string(mime_dir.join("globs")) {
            Ok(t) => (t, false),
            Err(_) => return (out, no_globs),
        },
    };
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (weight, mime, pattern, cs) = if v2 {
            let mut fields = line.splitn(4, ':');
            let (Some(w), Some(m), Some(p)) = (fields.next(), fields.next(), fields.next()) else {
                continue;
            };
            let Ok(w) = w.parse::<u32>() else { continue };
            // Unknown flags and any further fields are reserved for
            // future use and must be ignored.
            let flags = fields.next().unwrap_or("").split(':').next().unwrap_or("");
            (w, m, p, flags.split(',').any(|f| f == "cs"))
        } else {
            let Some((m, p)) = line.split_once(':') else {
                continue;
            };
            (50, m, p, false)
        };
        if mime.is_empty() || pattern.is_empty() {
            continue;
        }
        if pattern == "__NOGLOBS__" {
            no_globs.insert(mime.to_string());
            continue;
        }
        out.push((weight, mime.to_string(), pattern.to_string(), cs));
    }
    (out, no_globs)
}

/// `aliases` and `subclasses` share the "first second" line format.
fn read_pairs(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect()
}

/// fnmatch(3) without flags: `*`, `?`, `[...]` (with `!`/`^` negation and
/// ranges) and backslash escapes.
fn fnmatch(pattern: &[char], name: &[char]) -> bool {
    let (mut pi, mut ni) = (0, 0);
    // Where to resume after the last `*` when a later part fails to match.
    let mut star: Option<(usize, usize)> = None;
    while ni < name.len() {
        let next = if pi < pattern.len() {
            match pattern[pi] {
                '*' => {
                    star = Some((pi, ni));
                    pi += 1;
                    continue;
                }
                '?' => Some(pi + 1),
                '[' => match match_class(pattern, pi, name[ni]) {
                    Some((true, after)) => Some(after),
                    Some((false, _)) => None,
                    // No closing bracket: the `[` is literal.
                    None => (name[ni] == '[').then_some(pi + 1),
                },
                '\\' if pi + 1 < pattern.len() => (pattern[pi + 1] == name[ni]).then_some(pi + 2),
                c => (c == name[ni]).then_some(pi + 1),
            }
        } else {
            None
        };
        match (next, star) {
            (Some(p), _) => {
                pi = p;
                ni += 1;
            }
            (None, Some((sp, sn))) => {
                pi = sp + 1;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            }
            (None, None) => return false,
        }
    }
    pattern[pi..].iter().all(|&c| c == '*')
}

/// Match `c` against the bracket expression starting at `pattern[start]`
/// (`[`); returns whether it matched and the index after `]`.
fn match_class(pattern: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(pattern.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let lo = *pattern.get(i)?;
        if lo == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|&h| h != ']') {
            let hi = pattern[i + 2];
            matched |= lo <= c && c <= hi;
            i += 3;
        } else {
            matched |= lo == c;
            i += 1;
        }
    }
}

enum MagicLine {
    Rule(Matchlet, usize),
    NoMagic(usize),
    Skip(usize),
    Truncated,
}

/// Parse the binary `magic` file into sections, plus the types this
/// directory marks `__NOMAGIC__`.
fn read_magic(path: &Path) -> (Vec<MagicSection>, HashSet<String>) {
    const HEADER: &[u8] = b"MIME-Magic\0\n";
    let mut sections = Vec::new();
    let mut no_magic = HashSet::new();
    let Ok(data) = fs::read(path) else {
        return (sections, no_magic);
    };
    if !data.starts_with(HEADER) {
        return (sections, no_magic);
    }
    let mut pos = HEADER.len();
    let mut current: Option<MagicSection> = None;
    while pos < data.len() {
        if data[pos] == b'[' {
            let Some(end) = find_byte(&data, pos, b'\n') else {
                break;
            };
            sections.extend(current.take());
            current = std::str::from_utf8(&data[pos + 1..end])
                .ok()
                .and_then(|h| h.strip_suffix(']'))
                .and_then(|h| h.split_once(':'))
                .and_then(|(prio, mime)| {
                    Some(MagicSection {
                        priority: prio.parse().ok()?,
                        mime: mime.to_string(),
                        rules: Vec::new(),
                    })
                });
            pos = end + 1;
            continue;
        }
        match parse_magic_line(&data, pos) {
            MagicLine::Rule(m, next) => {
                if let Some(s) = current.as_mut() {
                    s.rules.push(m);
                }
                pos = next;
            }
            MagicLine::NoMagic(next) => {
                if let Some(s) = current.as_ref() {
                    no_magic.insert(s.mime.clone());
                }
                pos = next;
            }
            MagicLine::Skip(next) => pos = next,
            MagicLine::Truncated => break,
        }
    }
    sections.extend(current);
    sections.retain(|s| !s.rules.is_empty());
    (sections, no_magic)
}

fn find_byte(data: &[u8], from: usize, b: u8) -> Option<usize> {
    data[from..].iter().position(|&x| x == b).map(|i| from + i)
}

fn read_decimal(data: &[u8], pos: &mut usize) -> Option<usize> {
    let start = *pos;
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        *pos += 1;
    }
    std::str::from_utf8(&data[start..*pos]).ok()?.parse().ok()
}

/// `[indent] ">" offset "=" value ["&" mask] ["~" word-size] ["+" range] "\n"`
fn parse_magic_line(data: &[u8], start: usize) -> MagicLine {
    // Unknown content: per the spec there is no binary data after it, so the
    // line ends at the next newline.
    let skip = |from: usize| match find_byte(data, from, b'\n') {
        Some(nl) => MagicLine::Skip(nl + 1),
        None => MagicLine::Truncated,
    };
    let mut i = start;
    let indent = read_decimal(data, &mut i).unwrap_or(0) as u32;
    if data.get(i) != Some(&b'>') {
        return skip(i);
    }
    i += 1;
    let Some(offset) = read_decimal(data, &mut i) else {
        return skip(i);
    };
    if data.get(i) != Some(&b'=') {
        return skip(i);
    }
    i += 1;
    // The deleteall marker is written as text, not as a length-prefixed value.
    if data[i..].starts_with(b"__NOMAGIC__\n") {
        return MagicLine::NoMagic(i + 12);
    }
    if i + 2 > data.len() {
        return MagicLine::Truncated;
    }
    let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
    i += 2;
    if i + len > data.len() {
        return MagicLine::Truncated;
    }
    let mut value = data[i..i + len].to_vec();
    i += len;
    let mut mask = None;
    if data.get(i) == Some(&b'&') {
        i += 1;
        if i + len > data.len() {
            return MagicLine::Truncated;
        }
        mask = Some(data[i..i + len].to_vec());
        i += len;
    }
    let mut word_size = 1;
    if data.get(i) == Some(&b'~') {
        i += 1;
        match read_decimal(data, &mut i) {
            Some(w) => word_size = w,
            None => return skip(i),
        }
    }
    let mut range = 1;
    if data.get(i) == Some(&b'+') {
        i += 1;
        match read_decimal(data, &mut i) {
            Some(r) => range = r.max(1),
            None => return skip(i),
        }
    }
    if data.get(i) != Some(&b'\n') {
        return skip(i);
    }
    // Values are stored big-endian; "host" rules (word size > 1) must be
    // swapped to match native data on little-endian machines.
    if cfg!(target_endian = "little") && word_size > 1 && len.is_multiple_of(word_size) {
        for chunk in value.chunks_mut(word_size) {
            chunk.reverse();
        }
        if let Some(m) = mask.as_mut() {
            for chunk in m.chunks_mut(word_size) {
                chunk.reverse();
            }
        }
    }
    MagicLine::Rule(
        Matchlet {
            indent,
            offset,
            range,
            value,
            mask,
        },
        i + 1,
    )
}

impl Matchlet {
    fn matches(&self, data: &[u8]) -> bool {
        let len = self.value.len();
        for start in self.offset..self.offset + self.range {
            let end = start + len;
            if end > data.len() {
                return false;
            }
            let window = &data[start..end];
            let hit = match &self.mask {
                None => window == self.value.as_slice(),
                Some(mask) => window
                    .iter()
                    .zip(&self.value)
                    .zip(mask)
                    .all(|((d, v), m)| d & m == v & m),
            };
            if hit {
                return true;
            }
        }
        false
    }
}

/// A section matches when any top-level rule matches together with (if it
/// has any) at least one of its children, recursively.
fn section_matches(rules: &[Matchlet], data: &[u8]) -> bool {
    (0..rules.len()).any(|i| rules[i].indent == 0 && rule_tree_matches(rules, i, data))
}

fn rule_tree_matches(rules: &[Matchlet], i: usize, data: &[u8]) -> bool {
    if !rules[i].matches(data) {
        return false;
    }
    let indent = rules[i].indent;
    let mut has_child = false;
    for j in i + 1..rules.len() {
        if rules[j].indent <= indent {
            break;
        }
        if rules[j].indent == indent + 1 {
            has_child = true;
            if rule_tree_matches(rules, j, data) {
                return true;
            }
        }
    }
    !has_child
}

// ---------------------------------------------------------------------------
// Process environment used while loading
// ---------------------------------------------------------------------------

/// What loading reads from the process environment, gathered in one place
/// so tests can supply their own instead of mutating global env vars.
struct Env {
    path: Vec<PathBuf>,
    /// `Name[xx_YY@mod]`-style keys to try, most specific first.
    name_keys: Vec<String>,
    terminal: Option<String>,
    home: Option<PathBuf>,
    found: RefCell<HashMap<String, bool>>,
}

impl Env {
    fn from_process() -> Env {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|v| env_nonempty(v));
        Env::new(
            std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            locale.as_deref(),
            env_nonempty("TERMINAL"),
            env_nonempty("HOME").map(PathBuf::from),
        )
    }

    fn new(
        path: Vec<PathBuf>,
        locale: Option<&str>,
        terminal: Option<String>,
        home: Option<PathBuf>,
    ) -> Env {
        Env {
            path,
            name_keys: locale_variants(locale)
                .into_iter()
                .map(|l| format!("Name[{l}]"))
                .collect(),
            terminal,
            home,
            found: RefCell::new(HashMap::new()),
        }
    }

    /// Whether `program` resolves to an executable file, either as a path
    /// or through `$PATH`. Cached: hundreds of entries share a few programs.
    fn has_program(&self, program: &str) -> bool {
        if let Some(&known) = self.found.borrow().get(program) {
            return known;
        }
        let found = if program.contains('/') {
            is_executable_file(Path::new(program))
        } else {
            self.path
                .iter()
                .any(|d| is_executable_file(&d.join(program)))
        };
        self.found.borrow_mut().insert(program.to_string(), found);
        found
    }
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && has_exec_bit(&m))
}

#[cfg(unix)]
fn has_exec_bit(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_exec_bit(_meta: &fs::Metadata) -> bool {
    false
}

/// Locale keys in the desktop entry spec's matching order: for
/// `lang_COUNTRY.ENCODING@MODIFIER` that is `lang_COUNTRY@MODIFIER`,
/// `lang_COUNTRY`, `lang@MODIFIER`, `lang` (the encoding is ignored).
fn locale_variants(locale: Option<&str>) -> Vec<String> {
    let Some(locale) = locale else {
        return Vec::new();
    };
    let (rest, modifier) = match locale.split_once('@') {
        Some((r, m)) => (r, Some(m)),
        None => (locale, None),
    };
    let rest = rest.split('.').next().unwrap_or("");
    if rest.is_empty() || rest == "C" || rest == "POSIX" {
        return Vec::new();
    }
    let (lang, country) = match rest.split_once('_') {
        Some((l, c)) => (l, Some(c)),
        None => (rest, None),
    };
    let mut out = Vec::new();
    match (country, modifier) {
        (Some(c), Some(m)) => {
            out.push(format!("{lang}_{c}@{m}"));
            out.push(format!("{lang}_{c}"));
            out.push(format!("{lang}@{m}"));
        }
        (Some(c), None) => out.push(format!("{lang}_{c}")),
        (None, Some(m)) => out.push(format!("{lang}@{m}")),
        (None, None) => {}
    }
    out.push(lang.to_string());
    out
}

// ---------------------------------------------------------------------------
// Key files (desktop entries, mimeapps.list, kdeglobals)
// ---------------------------------------------------------------------------

/// The keys of one `[group]` of an ini-style file. The first occurrence of
/// a key wins; spaces around `=` are ignored.
fn parse_group<'a>(text: &'a str, header: &str) -> HashMap<&'a str, &'a str> {
    let mut keys = HashMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if inside {
                break;
            }
            inside = line == header;
            continue;
        }
        if inside {
            if let Some((k, v)) = line.split_once('=') {
                keys.entry(k.trim()).or_insert(v.trim());
            }
        }
    }
    keys
}

/// Undo the desktop entry spec's string escapes (`\s \n \t \r \\`).
/// Unknown escapes are kept as written, which is what the Exec quoting
/// layer expects for `\"`, `\$` and friends.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A `;`-separated list value, where `\;` is a literal semicolon.
fn split_list(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => cur.push(';'),
                Some(other) => {
                    cur.push('\\');
                    cur.push(other);
                }
                None => {}
            },
            ';' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| unescape(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_bool(value: &str) -> bool {
    // "1" is not in the current spec, but old KDE files still use it.
    value == "true" || value == "1"
}

// ---------------------------------------------------------------------------
// Exec lines and field codes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Lit(String),
    /// A field code; `quoted` when it sat inside a quoted string.
    Code {
        code: char,
        quoted: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Arg {
    pieces: Vec<Piece>,
    quoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Dialect {
    /// Desktop entry Exec: every `%x` must be a known field code.
    Desktop,
    /// A command typed by the user: only `%f %F %u %U %%` are special, so
    /// `date +%Y` keeps working.
    Typed,
}

#[derive(Default)]
struct ArgBuilder {
    args: Vec<Arg>,
    pieces: Vec<Piece>,
    lit: String,
    started: bool,
    quoted: bool,
}

impl ArgBuilder {
    fn push_char(&mut self, c: char) {
        self.lit.push(c);
        self.started = true;
    }
    fn push_code(&mut self, code: char, quoted: bool) {
        self.flush_lit();
        self.pieces.push(Piece::Code { code, quoted });
        self.started = true;
    }
    fn mark_quoted(&mut self) {
        self.started = true;
        self.quoted = true;
    }
    fn flush_lit(&mut self) {
        if !self.lit.is_empty() {
            self.pieces.push(Piece::Lit(std::mem::take(&mut self.lit)));
        }
    }
    fn end_arg(&mut self) {
        if self.started {
            self.flush_lit();
            self.args.push(Arg {
                pieces: std::mem::take(&mut self.pieces),
                quoted: self.quoted,
            });
        }
        self.started = false;
        self.quoted = false;
    }
}

/// Split a command line into arguments.
///
/// Implements the Exec quoting rules (double quotes, inside which `\"`,
/// `` \` ``, `\$` and `\\` are escapes). It is deliberately lenient where
/// GLib and KDE are: single quotes and backslashes outside quotes work as
/// in a shell, since real desktop files use them.
fn tokenize(line: &str, dialect: Dialect) -> Result<Vec<Arg>, String> {
    let mut b = ArgBuilder::default();
    let mut chars = line.chars().peekable();
    let (mut in_double, mut in_single) = (false, false);
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                b.push_char(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_double = !in_double;
                b.mark_quoted();
            }
            '\\' if in_double => match chars.peek() {
                Some(&n) if matches!(n, '"' | '`' | '$' | '\\') => {
                    b.push_char(n);
                    chars.next();
                }
                _ => b.push_char('\\'),
            },
            '%' => {
                let next = chars.peek().copied();
                match (dialect, next) {
                    (_, Some('%')) => {
                        chars.next();
                        b.push_char('%');
                    }
                    (_, Some(code @ ('f' | 'F' | 'u' | 'U'))) => {
                        chars.next();
                        b.push_code(code, in_double);
                    }
                    (Dialect::Desktop, Some(code @ ('i' | 'c' | 'k'))) => {
                        chars.next();
                        b.push_code(code, in_double);
                    }
                    // Deprecated codes are removed and ignored.
                    (Dialect::Desktop, Some('d' | 'D' | 'n' | 'N' | 'v' | 'm')) => {
                        chars.next();
                    }
                    (Dialect::Desktop, Some(other)) => {
                        return Err(format!("unknown field code %{other}"));
                    }
                    (Dialect::Desktop, None) => return Err("lone % at the end".to_string()),
                    (Dialect::Typed, _) => b.push_char('%'),
                }
            }
            _ if in_double => b.push_char(c),
            ' ' | '\t' | '\n' => b.end_arg(),
            '\'' => {
                in_single = true;
                b.mark_quoted();
            }
            '\\' => match chars.next() {
                Some(n) => b.push_char(n),
                None => b.push_char('\\'),
            },
            _ => b.push_char(c),
        }
    }
    if in_double || in_single {
        return Err("unterminated quote".to_string());
    }
    b.end_arg();
    Ok(b.args)
}

/// What `%i`, `%c` and `%k` expand to.
#[derive(Default)]
struct ExpandContext<'a> {
    name: &'a str,
    icon: Option<&'a str>,
    desktop_file: Option<&'a Path>,
}

fn codes(args: &[Arg]) -> impl Iterator<Item = (char, bool)> + '_ {
    args.iter().flat_map(|a| {
        let alone = a.pieces.len() == 1;
        a.pieces.iter().filter_map(move |p| match p {
            Piece::Code { code, .. } => Some((*code, alone)),
            Piece::Lit(_) => None,
        })
    })
}

/// Expand an Exec line into one argv per invocation: `%F`/`%U` take every
/// file in one run; `%f`/`%u` mean the program takes a single file, so it
/// runs once per file.
fn expand_invocations(args: &[Arg], files: &[PathBuf], ctx: &ExpandContext) -> Vec<Vec<String>> {
    // Only a standalone %F/%U can carry several files; anywhere else it is
    // invalid per the spec and treated like %f/%u.
    let multi = codes(args).any(|(c, alone)| alone && matches!(c, 'F' | 'U'));
    let single = codes(args).any(|(c, _)| matches!(c, 'f' | 'u' | 'F' | 'U'));
    let runs: Vec<Vec<String>> = if files.is_empty() || multi {
        vec![expand_args(args, files, ctx)]
    } else if single {
        files
            .iter()
            .map(|f| expand_args(args, std::slice::from_ref(f), ctx))
            .collect()
    } else {
        // No file field code: append the file, once per file, which is how
        // GLib and KIO treat such entries (as if the line ended in %f).
        files
            .iter()
            .map(|f| {
                let mut argv = expand_args(args, &[], ctx);
                argv.push(path_arg(f));
                argv
            })
            .collect()
    };
    runs.into_iter().filter(|argv| !argv.is_empty()).collect()
}

fn expand_args(args: &[Arg], files: &[PathBuf], ctx: &ExpandContext) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        if let [Piece::Code { code, .. }] = arg.pieces.as_slice() {
            // A code that is the whole argument may expand to zero or
            // several arguments.
            match code {
                'f' => out.extend(files.first().map(|f| path_arg(f))),
                'F' => out.extend(files.iter().map(|f| path_arg(f))),
                'u' => out.extend(files.first().map(|f| file_uri(f))),
                'U' => out.extend(files.iter().map(|f| file_uri(f))),
                'i' => {
                    if let Some(icon) = ctx.icon.filter(|i| !i.is_empty()) {
                        out.push("--icon".to_string());
                        out.push(icon.to_string());
                    }
                }
                'c' => out.push(ctx.name.to_string()),
                'k' => out.extend(ctx.desktop_file.map(path_arg)),
                _ => {}
            }
            continue;
        }
        let mut s = String::new();
        let mut has_literal = false;
        for piece in &arg.pieces {
            match piece {
                Piece::Lit(l) => {
                    s.push_str(l);
                    has_literal = true;
                }
                Piece::Code { code, quoted } => {
                    let value = match code {
                        'f' | 'F' => files.first().map(|f| path_arg(f)),
                        'u' | 'U' => files.first().map(|f| file_uri(f)),
                        'i' => ctx.icon.map(str::to_string),
                        'c' => Some(ctx.name.to_string()),
                        'k' => ctx.desktop_file.map(path_arg),
                        _ => None,
                    }
                    .unwrap_or_default();
                    // Codes inside quotes are undefined by the spec; in
                    // practice that is `sh -c "prog %f"`, so the value is
                    // quoted for the shell, as GLib and KIO do.
                    if *quoted && !value.is_empty() {
                        s.push_str(&shell_quote(&value));
                    } else {
                        s.push_str(&value);
                    }
                }
            }
        }
        if s.is_empty() && !has_literal && !arg.quoted {
            continue;
        }
        out.push(s);
    }
    out
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_arg(path: &Path) -> String {
    absolute(path).to_string_lossy().into_owned()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `file://` URI for a local path, percent-encoding every byte outside the
/// unreserved set (so non-UTF-8 names survive intact).
pub fn file_uri(path: &Path) -> String {
    let abs = absolute(path);
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        abs.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = abs.to_string_lossy().replace('\\', "/").into_bytes();
    let mut uri = String::from("file://");
    for b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// Argv for a command the user typed in "Open with → Other…".
///
/// Shell-like quoting (single quotes, double quotes, backslashes). `%f`,
/// `%F`, `%u` and `%U` place the files; without any of them the files are
/// appended. Only one command comes back, so a standalone `%f`/`%u` takes
/// all the files like `%F`/`%U`. Fails on an empty line, unbalanced
/// quotes, a code embedded in an argument with several files (no single
/// argv can express that), or a program that is not found.
pub fn custom_command(line: &str, files: &[PathBuf]) -> Result<Vec<String>, String> {
    custom_command_with(line, files, &Env::from_process())
}

fn custom_command_with(line: &str, files: &[PathBuf], env: &Env) -> Result<Vec<String>, String> {
    let mut args = tokenize(line, Dialect::Typed)?;
    if args.is_empty() {
        return Err("empty command".to_string());
    }
    let has_code = codes(&args).next().is_some();
    if files.len() > 1 && codes(&args).any(|(_, alone)| !alone) {
        return Err("%f/%u inside an argument can only take one file".to_string());
    }
    let ctx = ExpandContext::default();
    let argv = if has_code {
        for arg in &mut args {
            if let [Piece::Code { code, .. }] = arg.pieces.as_mut_slice() {
                *code = code.to_ascii_uppercase();
            }
        }
        expand_args(&args, files, &ctx)
    } else {
        let mut argv = expand_args(&args, &[], &ctx);
        argv.extend(files.iter().map(|f| path_arg(f)));
        argv
    };
    let Some(program) = argv.first() else {
        return Err("empty command".to_string());
    };
    if program.is_empty() || !env.has_program(program) {
        return Err(format!("{program}: command not found"));
    }
    Ok(argv)
}

// ---------------------------------------------------------------------------
// Applications
// ---------------------------------------------------------------------------

/// An application from a desktop entry.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    /// Desktop file ID, e.g. `org.kde.kate.desktop`.
    pub id: String,
    /// `Name`, localized for `LC_ALL`/`LC_MESSAGES`/`LANG`.
    pub name: String,
    pub icon: Option<String>,
    /// The `Exec` value with the string escapes already undone (the Exec
    /// quoting and field codes are handled by [`App::commands`]).
    pub exec: String,
    pub terminal: bool,
    /// The desktop file this came from (what `%k` expands to).
    pub path: PathBuf,
    /// `MimeType` as written in the file.
    pub mime_types: Vec<String>,
    /// `NoDisplay=true`: hidden from menus but still valid for files.
    pub no_display: bool,
}

impl App {
    /// The argv lists to run to open `files` (or to just start the app when
    /// `files` is empty). `%f`/`%u` give one invocation per file, `%F`/`%U`
    /// a single one; URIs are percent-encoded `file://` URIs. Terminal apps
    /// are not wrapped here; see [`Apps::launch_commands`].
    pub fn commands(&self, files: &[PathBuf]) -> Vec<Vec<String>> {
        let Ok(args) = tokenize(&self.exec, Dialect::Desktop) else {
            return Vec::new();
        };
        let ctx = ExpandContext {
            name: &self.name,
            icon: self.icon.as_deref(),
            desktop_file: Some(self.path.as_path()).filter(|p| !p.as_os_str().is_empty()),
        };
        expand_invocations(&args, files, &ctx)
    }
}

/// Association data from the mimeapps.list files of one directory.
#[derive(Debug, Default, Clone)]
struct MimeAppsLevel {
    /// Desktop-specific files' entries first, then mimeapps.list's.
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
}

impl MimeAppsLevel {
    fn read(dir: &Path, desktops: &[String]) -> MimeAppsLevel {
        let mut level = MimeAppsLevel::default();
        for desktop in desktops {
            if let Ok(text) = fs::read_to_string(dir.join(format!("{desktop}-mimeapps.list"))) {
                // The spec only allows defaults in desktop-specific files.
                level.parse(&text, true);
            }
        }
        if let Ok(text) = fs::read_to_string(dir.join("mimeapps.list")) {
            level.parse(&text, false);
        }
        level
    }

    fn parse(&mut self, text: &str, defaults_only: bool) {
        #[derive(Clone, Copy)]
        enum Group {
            Defaults,
            Added,
            Removed,
        }
        let mut group = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                group = match line {
                    "[Default Applications]" => Some(Group::Defaults),
                    "[Added Associations]" if !defaults_only => Some(Group::Added),
                    "[Removed Associations]" if !defaults_only => Some(Group::Removed),
                    _ => None,
                };
                continue;
            }
            let Some(group) = group else { continue };
            let Some((mime, ids)) = line.split_once('=') else {
                continue;
            };
            let table = match group {
                Group::Defaults => &mut self.defaults,
                Group::Added => &mut self.added,
                Group::Removed => &mut self.removed,
            };
            table
                .entry(mime.trim().to_string())
                .or_default()
                .extend(split_list(ids));
        }
    }
}

/// Every installed application and every mimeapps.list, loaded once.
#[derive(Debug, Default, Clone)]
pub struct Apps {
    apps: Vec<App>,
    /// Precedence level (index into `levels`) of each app's desktop file.
    app_level: Vec<usize>,
    by_id: HashMap<String, usize>,
    /// Apps listing a type in `MimeType`, in level then file order.
    by_mime: HashMap<String, Vec<usize>>,
    /// Config dirs first, then data dirs, i.e. the mimeapps.list order.
    levels: Vec<MimeAppsLevel>,
    terminal: Option<Vec<String>>,
}

impl Apps {
    /// Load desktop entries from `<data_dir>/applications` and mimeapps.list
    /// files from the config dirs and data dirs, all in precedence order
    /// (home first). `desktop` is the `XDG_CURRENT_DESKTOP` value (a
    /// colon-separated list); each name, lowercased, selects
    /// `<name>-mimeapps.list` (`KDE` → `kde-mimeapps.list`).
    pub fn load(config_dirs: &[PathBuf], data_dirs: &[PathBuf], desktop: Option<&str>) -> Apps {
        Apps::load_with(config_dirs, data_dirs, desktop, &Env::from_process())
    }

    /// [`Apps::load`] with the XDG dirs and desktop of the environment.
    pub fn from_env() -> Apps {
        Apps::load(
            &xdg_config_dirs(),
            &xdg_data_dirs(),
            env_nonempty("XDG_CURRENT_DESKTOP").as_deref(),
        )
    }

    fn load_with(
        config_dirs: &[PathBuf],
        data_dirs: &[PathBuf],
        desktop: Option<&str>,
        env: &Env,
    ) -> Apps {
        let config_dirs = dedup_paths(config_dirs);
        let data_dirs = dedup_paths(data_dirs);
        let desktops: Vec<String> = desktop
            .unwrap_or("")
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| d.to_ascii_lowercase())
            .collect();

        let mut apps = Apps::default();
        for dir in &config_dirs {
            apps.levels.push(MimeAppsLevel::read(dir, &desktops));
        }
        // Every ID seen so far, usable or not: a hidden or broken entry in
        // a higher directory still shadows the same ID further down.
        let mut seen: HashSet<String> = HashSet::new();
        for dir in &data_dirs {
            let level = apps.levels.len();
            let app_dir = dir.join("applications");
            apps.levels.push(MimeAppsLevel::read(&app_dir, &desktops));
            let mut files = Vec::new();
            collect_desktop_files(&app_dir, "", 0, &mut files);
            for (id, path) in files {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(app) = load_entry(&path, id, env) {
                    let index = apps.apps.len();
                    for mime in &app.mime_types {
                        apps.by_mime.entry(mime.clone()).or_default().push(index);
                    }
                    apps.by_id.insert(app.id.clone(), index);
                    apps.apps.push(app);
                    apps.app_level.push(level);
                }
            }
        }
        apps.terminal = find_terminal_with(&config_dirs, env);
        apps
    }

    /// Every usable application (including `NoDisplay` ones).
    pub fn all(&self) -> &[App] {
        &self.apps
    }

    /// The application with this desktop file ID.
    pub fn get(&self, id: &str) -> Option<&App> {
        self.by_id.get(id).map(|&i| &self.apps[i])
    }

    /// The terminal prefix found at load time (e.g. `["konsole", "-e"]`).
    pub fn terminal(&self) -> Option<&[String]> {
        self.terminal.as_deref()
    }

    /// The application that opens `mime` on Enter.
    ///
    /// Per the mime-apps spec, for the type and then each ancestor (nearest
    /// first): the first installed entry of `[Default Applications]` across
    /// the mimeapps.list files in precedence order, else the most preferred
    /// associated application. So an app that handles `text/x-rust` itself
    /// beats the default for `text/plain`, which in turn beats a hex editor
    /// registered for `application/octet-stream`.
    pub fn default_for(&self, mime: &str, db: &MimeDb) -> Option<&App> {
        self.default_index(mime, db).map(|i| &self.apps[i])
    }

    /// Every application that can open `mime`, best first: the default,
    /// then the associations of the type itself (Added Associations and
    /// desktop files, interleaved by directory as the spec describes), then
    /// those of each ancestor. Apps removed for the type are left out
    /// entirely, even when an ancestor would bring them back.
    pub fn for_type(&self, mime: &str, db: &MimeDb) -> Vec<&App> {
        let canonical = db.unalias(mime);
        let removed = self.removed_for_type(&db.names_of(canonical));
        let mut out: Vec<usize> = self.default_index(mime, db).into_iter().collect();
        for t in db.ancestors(canonical) {
            for i in self.associated(&db.names_of(&t)) {
                if !removed.contains(&i) && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out.into_iter().map(|i| &self.apps[i]).collect()
    }

    /// The full argv lists to run `app` on `files`: [`App::commands`],
    /// wrapped in the terminal for `Terminal=true` apps. Fails only when a
    /// terminal is needed and none was found.
    pub fn launch_commands(
        &self,
        app: &App,
        files: &[PathBuf],
    ) -> Result<Vec<Vec<String>>, String> {
        let commands = app.commands(files);
        if !app.terminal {
            return Ok(commands);
        }
        let terminal = self.terminal.as_ref().ok_or_else(|| {
            format!(
                "{} needs a terminal, and no terminal emulator was found",
                app.name
            )
        })?;
        Ok(commands
            .into_iter()
            .map(|argv| terminal.iter().cloned().chain(argv).collect())
            .collect())
    }

    fn default_index(&self, mime: &str, db: &MimeDb) -> Option<usize> {
        let canonical = db.unalias(mime);
        let removed_exact = self.removed_for_type(&db.names_of(canonical));
        for t in db.ancestors(canonical) {
            let names = db.names_of(&t);
            let associated = self.associated(&names);
            for (l, level) in self.levels.iter().enumerate() {
                for name in &names {
                    for id in level.defaults.get(*name).into_iter().flatten() {
                        let Some(&i) = self.by_id.get(id) else {
                            continue;
                        };
                        // The spec wants defaults to be associated with the
                        // type. That is not required here (`xdg-mime default`
                        // writes only the default, and GLib honours it), but
                        // an explicit removal at this level or above still
                        // disqualifies one.
                        let removed_here = self.removed_apps(&names, l + 1).contains(&i)
                            && !associated.contains(&i);
                        if removed_exact.contains(&i) || removed_here {
                            continue;
                        }
                        return Some(i);
                    }
                }
            }
            if let Some(i) = associated.into_iter().find(|i| !removed_exact.contains(i)) {
                return Some(i);
            }
        }
        None
    }

    /// Apps that `[Removed Associations]` in the first `up_to` levels take
    /// away from `names`. A removal only reaches desktop files at its own
    /// level or below, like an addition.
    fn removed_apps(&self, names: &[&str], up_to: usize) -> HashSet<usize> {
        let mut out = HashSet::new();
        for (l, level) in self.levels.iter().enumerate().take(up_to) {
            for name in names {
                for id in level.removed.get(*name).into_iter().flatten() {
                    if let Some(&i) = self.by_id.get(id) {
                        if self.app_level[i] >= l {
                            out.insert(i);
                        }
                    }
                }
            }
        }
        out
    }

    /// Apps removed for the queried type itself. They must not come back
    /// through a parent type (removing Kate for text/x-rust should not leave
    /// it listed because it handles text/plain), unless a higher level
    /// re-added them for this very type.
    fn removed_for_type(&self, names: &[&str]) -> HashSet<usize> {
        let associated = self.associated(names);
        let mut removed = self.removed_apps(names, self.levels.len());
        removed.retain(|i| !associated.contains(i));
        removed
    }

    /// The mime-apps spec's association algorithm for one type: per
    /// directory, its Added Associations (minus anything blacklisted so
    /// far), then its Removed Associations join the blacklist, then its own
    /// desktop files that list the type. An addition only counts for a
    /// desktop file at this level or below (one in a higher directory is a
    /// different file that the addition never saw).
    fn associated(&self, names: &[&str]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut blacklist: HashSet<&str> = HashSet::new();
        for (l, level) in self.levels.iter().enumerate() {
            for name in names {
                for id in level.added.get(*name).into_iter().flatten() {
                    if blacklist.contains(id.as_str()) {
                        continue;
                    }
                    if let Some(&i) = self.by_id.get(id) {
                        if self.app_level[i] >= l && !out.contains(&i) {
                            out.push(i);
                        }
                    }
                }
            }
            for name in names {
                blacklist.extend(
                    level
                        .removed
                        .get(*name)
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            }
            for name in names {
                for &i in self.by_mime.get(*name).into_iter().flatten() {
                    if self.app_level[i] == l
                        && !blacklist.contains(self.apps[i].id.as_str())
                        && !out.contains(&i)
                    {
                        out.push(i);
                    }
                }
            }
        }
        out
    }
}

/// `(desktop file ID, path)` for every `.desktop` file under `dir`; files in
/// subdirectories get IDs with `-` for `/` (`kde/foo.desktop` → `kde-foo.desktop`).
fn collect_desktop_files(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    // Symlinked directories are followed (NixOS profiles are symlink
    // farms), so cap the depth against loops.
    if depth > 16 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    // Deterministic when foo-bar.desktop and foo/bar.desktop collide.
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let path = entry.path();
        let is_dir = match entry.file_type() {
            Ok(ft) if ft.is_symlink() => path.is_dir(),
            Ok(ft) => ft.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            collect_desktop_files(&path, &format!("{prefix}{name}-"), depth + 1, out);
        } else if name.ends_with(".desktop") {
            out.push((format!("{prefix}{name}"), path));
        }
    }
}

/// Parse one desktop file; `None` when it must not be offered: not an
/// application, `Hidden`, no usable `Exec` (an invalid field code makes the
/// whole line invalid per the spec), or its `TryExec` / `Exec` program is
/// missing (GLib applies the same Exec check, which hides leftovers of
/// uninstalled packages).
fn load_entry(path: &Path, id: String, env: &Env) -> Option<App> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let keys = parse_group(&text, "[Desktop Entry]");
    if keys.get("Type").copied() != Some("Application") {
        return None;
    }
    if keys.get("Hidden").is_some_and(|v| parse_bool(v)) {
        return None;
    }
    let exec = unescape(keys.get("Exec")?);
    let args = tokenize(&exec, Dialect::Desktop).ok()?;
    let program = match args.first()?.pieces.as_slice() {
        [Piece::Lit(p)] => Some(p.as_str()),
        _ => None,
    };
    if let Some(program) = program {
        if !env.has_program(program) {
            return None;
        }
    }
    if let Some(try_exec) = keys
        .get("TryExec")
        .map(|t| unescape(t))
        .filter(|t| !t.is_empty())
    {
        if !env.has_program(&try_exec) {
            return None;
        }
    }
    let name = env
        .name_keys
        .iter()
        .find_map(|k| keys.get(k.as_str()))
        .or_else(|| keys.get("Name"))
        .map(|n| unescape(n))
        .unwrap_or_else(|| id.trim_end_matches(".desktop").to_string());
    Some(App {
        name,
        icon: keys
            .get("Icon")
            .map(|i| unescape(i))
            .filter(|i| !i.is_empty()),
        exec,
        terminal: keys.get("Terminal").is_some_and(|v| parse_bool(v)),
        path: path.to_path_buf(),
        mime_types: keys
            .get("MimeType")
            .map(|m| split_list(m))
            .unwrap_or_default(),
        no_display: keys.get("NoDisplay").is_some_and(|v| parse_bool(v)),
        id,
    })
}

// ---------------------------------------------------------------------------
// Running files and terminals
// ---------------------------------------------------------------------------

/// Whether Enter on `path` should offer to run it rather than open it.
///
/// Requires a regular file with an execute bit, and then either an ELF
/// program or a script starting with `#!`. Everything else is false, even
/// with the x bit (every file on a FAT/NTFS mount has it): a JPEG, a text
/// file without a shebang, or a shared library. ELF libraries and PIE
/// programs share a header type, so the ELF itself decides: a program has
/// a dynamic loader (`PT_INTERP`) or the `DF_1_PIE` flag (static-pie).
pub fn is_executable(path: &Path, mime: &str, db: &MimeDb) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || !has_exec_bit(&meta) {
        return false;
    }
    let binary = [
        "application/x-executable",
        "application/x-pie-executable",
        "application/x-sharedlib",
    ]
    .iter()
    .any(|t| db.is_a(mime, t))
        || db.unalias(mime) == OCTET_STREAM;
    if binary {
        let Ok(head) = read_head(path, SNIFF_LEN) else {
            return false;
        };
        if head.starts_with(b"\x7fELF") {
            return elf_is_program(&head, path);
        }
        return head.starts_with(b"#!");
    }
    if db.is_a(mime, TEXT_PLAIN) {
        return read_head(path, 2).is_ok_and(|h| h == b"#!");
    }
    false
}

/// ET_EXEC, or ET_DYN with `PT_INTERP` or `DF_1_PIE` (see [`is_executable`]).
fn elf_is_program(head: &[u8], path: &Path) -> bool {
    const ET_EXEC: u16 = 2;
    const ET_DYN: u16 = 3;
    const PT_DYNAMIC: u32 = 2;
    const PT_INTERP: u32 = 3;
    const DT_FLAGS_1: u64 = 0x6fff_fffb;
    const DF_1_PIE: u64 = 0x0800_0000;

    if head.len() < 0x34 {
        return false;
    }
    let wide = head[4] == 2;
    let big = head[5] == 2;
    let num = |b: &[u8], off: usize, n: usize| -> Option<u64> {
        let bytes = b.get(off..off + n)?;
        let mut v = 0u64;
        for i in 0..n {
            let byte = if big { bytes[i] } else { bytes[n - 1 - i] };
            v = (v << 8) | u64::from(byte);
        }
        Some(v)
    };
    let word = if wide { 8 } else { 4 };
    let Some(e_type) = num(head, 16, 2) else {
        return false;
    };
    match e_type as u16 {
        ET_EXEC => return true,
        ET_DYN => {}
        _ => return false,
    }
    let (phoff, phentsize, phnum) = if wide {
        (num(head, 0x20, 8), num(head, 0x36, 2), num(head, 0x38, 2))
    } else {
        (num(head, 0x1c, 4), num(head, 0x2a, 2), num(head, 0x2c, 2))
    };
    let (Some(phoff), Some(phentsize), Some(phnum)) = (phoff, phentsize, phnum) else {
        return false;
    };
    let table_len = (phentsize * phnum) as usize;
    if phentsize < 0x20 || table_len > 1 << 16 {
        return false;
    }
    let Some(table) = read_range(head, path, phoff, table_len) else {
        return false;
    };
    let mut dynamic = None;
    for entry in table.chunks_exact(phentsize as usize) {
        match num(entry, 0, 4).map(|t| t as u32) {
            Some(PT_INTERP) => return true,
            Some(PT_DYNAMIC) => {
                dynamic = if wide {
                    num(entry, 8, 8).zip(num(entry, 32, 8))
                } else {
                    num(entry, 4, 4).zip(num(entry, 16, 4))
                };
            }
            _ => {}
        }
    }
    let Some((offset, size)) = dynamic else {
        return false;
    };
    let Some(section) = read_range(head, path, offset, (size as usize).min(1 << 16)) else {
        return false;
    };
    for entry in section.chunks_exact(2 * word) {
        let (Some(tag), Some(value)) = (num(entry, 0, word), num(entry, word, word)) else {
            break;
        };
        if tag == 0 {
            break;
        }
        if tag == DT_FLAGS_1 && value & DF_1_PIE != 0 {
            return true;
        }
    }
    false
}

/// Bytes `[offset, offset + len)` of the file, from `head` when it covers them.
fn read_range(head: &[u8], path: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
    let start = usize::try_from(offset).ok()?;
    if let Some(slice) = head.get(start..start.checked_add(len)?) {
        return Some(slice.to_vec());
    }
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Terminal emulators tried after `$TERMINAL` and KDE's setting, in order.
const TERMINALS: &[&str] = &[
    "konsole",
    "alacritty",
    "kitty",
    "foot",
    "wezterm",
    "ghostty",
    "gnome-terminal",
    "kgx",
    "xfce4-terminal",
    "mate-terminal",
    "terminator",
    "urxvt",
    "st",
    "x-terminal-emulator",
    "xterm",
];

/// The argv prefix that runs a command in a terminal, e.g.
/// `["konsole", "-e"]`: `$TERMINAL`, then `TerminalApplication` from the
/// `[General]` group of `kdeglobals` (the first config dir that sets it),
/// then konsole, then other common emulators. Only installed ones count.
pub fn find_terminal(config_dirs: &[PathBuf]) -> Option<Vec<String>> {
    find_terminal_with(config_dirs, &Env::from_process())
}

fn find_terminal_with(config_dirs: &[PathBuf], env: &Env) -> Option<Vec<String>> {
    let configured = [env.terminal.clone(), kde_terminal(config_dirs, env)];
    configured
        .into_iter()
        .flatten()
        .chain(TERMINALS.iter().map(|t| t.to_string()))
        .find_map(|cmd| terminal_argv(&cmd, env))
}

fn kde_terminal(config_dirs: &[PathBuf], env: &Env) -> Option<String> {
    config_dirs.iter().find_map(|dir| {
        let text = fs::read_to_string(dir.join("kdeglobals")).ok()?;
        let keys = parse_group(&text, "[General]");
        // KConfig may tag the key, e.g. `TerminalApplication[$e]` for
        // "expand variables"; `~` is the only expansion seen in practice.
        let (key, value) = keys.iter().find(|(k, _)| {
            **k == "TerminalApplication" || k.starts_with("TerminalApplication[$")
        })?;
        let mut value = unescape(value);
        if key.contains("$e") {
            if let (Some(rest), Some(home)) = (value.strip_prefix('~'), env.home.as_ref()) {
                value = format!("{}{rest}", home.display());
            }
        }
        Some(value).filter(|v| !v.trim().is_empty())
    })
}

/// `cmd` (possibly with arguments) plus the option that makes it run the
/// rest of its argv, when its program is installed.
fn terminal_argv(cmd: &str, env: &Env) -> Option<Vec<String>> {
    let mut argv = expand_args(
        &tokenize(cmd, Dialect::Typed).ok()?,
        &[],
        &ExpandContext::default(),
    );
    if argv.is_empty() || !env.has_program(&argv[0]) {
        return None;
    }
    let base = Path::new(&argv[0])
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let flag: &[&str] = match base.as_str() {
        "gnome-terminal" | "kgx" => &["--"],
        "xfce4-terminal" | "mate-terminal" | "terminator" => &["-x"],
        "wezterm" if argv.iter().any(|a| a == "start") => &["--"],
        "wezterm" => &["start", "--"],
        // These take the command as plain trailing arguments.
        "kitty" | "foot" => &[],
        _ => &["-e"],
    };
    // A configured "alacritty -e" must not become "alacritty -e -e".
    if !argv.ends_with(&flag.iter().map(|s| s.to_string()).collect::<Vec<_>>()) {
        argv.extend(flag.iter().map(|s| s.to_string()));
    }
    Some(argv)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A throwaway directory tree, removed on drop.
    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new() -> Tree {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "fractale-apps-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn write(&self, rel: &str, contents: impl AsRef<[u8]>) -> PathBuf {
            let path = self.path(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        #[cfg(unix)]
        fn write_exec(&self, rel: &str, contents: impl AsRef<[u8]>) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.write(rel, contents);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn rule(
        indent: u32,
        offset: usize,
        value: &[u8],
        mask: Option<&[u8]>,
        word: usize,
        range: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        if indent > 0 {
            out.extend(indent.to_string().bytes());
        }
        out.extend(format!(">{offset}=").bytes());
        out.extend((value.len() as u16).to_be_bytes());
        out.extend(value);
        if let Some(m) = mask {
            out.push(b'&');
            out.extend(m);
        }
        if word > 1 {
            out.extend(format!("~{word}").bytes());
        }
        if range > 1 {
            out.extend(format!("+{range}").bytes());
        }
        out.push(b'\n');
        out
    }

    fn magic(sections: &[(u32, &str, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut out = b"MIME-Magic\0\n".to_vec();
        for (prio, mime, rules) in sections {
            out.extend(format!("[{prio}:{mime}]\n").bytes());
            for r in rules {
                out.extend(r);
            }
        }
        out
    }

    /// A small but realistic MIME database under `<root>/share/mime`.
    fn mime_tree() -> (Tree, MimeDb) {
        let t = Tree::new();
        t.write(
            "share/mime/globs2",
            "# comment\n\
             80:text/html:*.html\n\
             50:application/gzip:*.gz\n\
             50:application/x-compressed-tar:*.tar.gz\n\
             50:text/x-csrc:*.c\n\
             50:text/x-c++src:*.C:cs\n\
             50:text/x-makefile:Makefile\n\
             50:text/x-makefile:*.mk\n\
             10:text/x-readme:README*\n\
             50:application/x-sharedlib:*.so.[0-9]*\n\
             50:image/png:*.png\n\
             50:image/jpeg:*.jpg\n\
             50:audio/x-amb:*.amb\n\
             50:video/x-amb:*.amb\n\
             50:text/x-twin:*.twin\n\
             50:application/x-twin:*.twin\n\
             60:text/x-heavy:*.dup\n\
             40:text/x-light:*.dup\n\
             50:text/x-rust:*.rs\n\
             50:image/svg+xml:*.svg\n\
             50:application/pdf:*.pdf\n\
             50:text/x-diff:*.diff\n\
             50:text/x-diff:*.patch\n\
             50:text/x-weird:*.wrd:cs,newflag:future:fields\n\
             60:text/x-python:*.py\n\
             50:text/x-python3:*.py\n",
        );
        t.write(
            "share/mime/magic",
            magic(&[
                (80, "video/x-amb", vec![rule(0, 0, b"AMBV", None, 1, 1)]),
                (
                    60,
                    "application/x-nested",
                    vec![
                        rule(0, 0, b"NEST", None, 1, 1),
                        rule(1, 4, b"OK", None, 1, 1),
                        rule(1, 4, b"GO", None, 1, 1),
                    ],
                ),
                (50, "image/png", vec![rule(0, 0, b"\x89PNG", None, 1, 1)]),
                (
                    50,
                    "application/x-ranged",
                    vec![rule(0, 0, b"needle", None, 1, 32)],
                ),
                (
                    50,
                    "application/x-masked",
                    vec![rule(0, 0, b"\xf0\x00", Some(b"\xf0\xff"), 1, 1)],
                ),
                (
                    50,
                    "application/x-host16",
                    vec![rule(0, 0, b"\x12\x34", None, 2, 1)],
                ),
                (
                    40,
                    "application/x-shellscript",
                    vec![rule(0, 0, b"#!/bin/sh", None, 1, 1)],
                ),
                (
                    40,
                    "text/x-python3",
                    vec![rule(0, 0, b"#!/usr/bin/env python3", None, 1, 1)],
                ),
            ]),
        );
        t.write(
            "share/mime/aliases",
            "application/x-pdf application/pdf\ntext/rust text/x-rust\n",
        );
        t.write(
            "share/mime/subclasses",
            "image/svg+xml application/xml\n\
             application/xml text/plain\n\
             application/x-shellscript text/plain\n\
             application/x-compressed-tar application/gzip\n",
        );
        let db = MimeDb::load(&[t.path("share")]);
        (t, db)
    }

    #[test]
    fn globs_weight_length_case_and_literals() {
        let (_t, db) = mime_tree();
        let name = |n: &str| db.type_of_name(n);
        // Longest pattern on equal weight.
        assert_eq!(
            name("Data.tar.gz").as_deref(),
            Some("application/x-compressed-tar")
        );
        assert_eq!(name("x.gz").as_deref(), Some("application/gzip"));
        // Case-insensitive by default...
        assert_eq!(name("INDEX.HTML").as_deref(), Some("text/html"));
        assert_eq!(
            name("ARCHIVE.TAR.GZ").as_deref(),
            Some("application/x-compressed-tar")
        );
        // ...but a case-sensitive glob wins for its exact spelling.
        assert_eq!(name("main.C").as_deref(), Some("text/x-c++src"));
        assert_eq!(name("main.c").as_deref(), Some("text/x-csrc"));
        // Unknown flags and extra fields are ignored, "cs" still applies.
        assert_eq!(name("a.wrd").as_deref(), Some("text/x-weird"));
        assert_eq!(name("a.WRD"), None);
        // Literal names, case-insensitive; they beat wildcard patterns.
        assert_eq!(name("Makefile").as_deref(), Some("text/x-makefile"));
        assert_eq!(name("makefile").as_deref(), Some("text/x-makefile"));
        // Generic fnmatch patterns.
        assert_eq!(name("README").as_deref(), Some("text/x-readme"));
        assert_eq!(name("README.whatever").as_deref(), Some("text/x-readme"));
        assert_eq!(
            name("libz.so.1.3").as_deref(),
            Some("application/x-sharedlib")
        );
        // A "*.ext" match beats a generic pattern (README.c is C).
        assert_eq!(name("README.c").as_deref(), Some("text/x-csrc"));
        // Higher weight wins outright.
        assert_eq!(name("a.dup").as_deref(), Some("text/x-heavy"));
        assert_eq!(name("nothing"), None);
        // Aliased types come back canonical: globs only use canonical names.
        assert_eq!(name("doc.pdf").as_deref(), Some("application/pdf"));
    }

    #[test]
    fn fnmatch_basics() {
        let m = |p: &str, n: &str| {
            fnmatch(
                &p.chars().collect::<Vec<_>>(),
                &n.chars().collect::<Vec<_>>(),
            )
        };
        assert!(m("*.so.[0-9]*", "libc.so.6"));
        assert!(!m("*.so.[0-9]*", "libc.so.x"));
        assert!(m("[!a]bc", "xbc"));
        assert!(!m("[!a]bc", "abc"));
        assert!(m("a?c", "abc"));
        assert!(m("*~", "file~"));
        assert!(m("[]]", "]"));
        assert!(m("a[", "a["));
        assert!(m("*a*b*", "xxaxxbxx"));
        assert!(!m("*a*b", "xxaxxbxxc"));
    }

    #[test]
    fn no_globs_in_higher_dir_hides_lower_globs() {
        let (t, _) = mime_tree();
        t.write(
            "home/mime/globs2",
            "0:text/x-diff:__NOGLOBS__\n50:text/x-diff:*.diff\n",
        );
        let db = MimeDb::load(&[t.path("home"), t.path("share")]);
        assert_eq!(db.type_of_name("a.diff").as_deref(), Some("text/x-diff"));
        assert_eq!(db.type_of_name("a.patch"), None);
        // Order matters: the same file in a lower dir does not hide anything.
        let db = MimeDb::load(&[t.path("share"), t.path("home")]);
        assert_eq!(db.type_of_name("a.patch").as_deref(), Some("text/x-diff"));
    }

    #[test]
    fn magic_sniffing() {
        let (t, db) = mime_tree();
        // No usable name: magic decides.
        let png = t.write("f/picture", b"\x89PNG\r\n\x1a\n rest");
        assert_eq!(db.type_of(&png), "image/png");
        // Nested rules need a matching child.
        assert_eq!(
            db.type_of(&t.write("f/n1", b"NESTGO..")),
            "application/x-nested"
        );
        assert_eq!(
            db.type_of(&t.write("f/n2", b"NESTNO\x00\x01")),
            OCTET_STREAM
        );
        // Ranges, masks, host-endian words.
        assert_eq!(
            db.type_of(&t.write("f/r", b"\x00\x01\x02 haystack needle")),
            "application/x-ranged"
        );
        assert_eq!(
            db.type_of(&t.write("f/m", b"\xf7\x00\x00")),
            "application/x-masked"
        );
        let host = if cfg!(target_endian = "little") {
            b"\x34\x12\x00"
        } else {
            b"\x12\x34\x00"
        };
        assert_eq!(db.type_of(&t.write("f/h", host)), "application/x-host16");
        // A unique glob is trusted without reading (README.mp3 is an MP3).
        assert_eq!(
            db.type_of(&t.write("f/fake.png", b"plain text")),
            "image/png"
        );
        // Tied globs: magic picks the one it recognises...
        assert_eq!(db.type_of(&t.write("f/a.amb", b"AMBV....")), "video/x-amb");
        // ...and without a magic match the first glob wins for binary data,
        assert_eq!(
            db.type_of(&t.write("f/b.amb", b"\x00\x01\x02")),
            "audio/x-amb"
        );
        // while text data picks the glob that is a text/plain.
        assert_eq!(db.type_of(&t.write("f/c.twin", b"hello\n")), "text/x-twin");
        assert_eq!(db.type_of(&t.write("f/d.twin", b"\x00\x01")), "text/x-twin");
        // No glob, no magic: the text heuristic (UTF-8 is still text).
        assert_eq!(
            db.type_of(&t.write("f/notes", "héllo wörld\n\tok\n")),
            TEXT_PLAIN
        );
        assert_eq!(
            db.type_of(&t.write("f/blob", b"\x00\x01\x02\x03")),
            OCTET_STREAM
        );
        // Scripts without an extension.
        assert_eq!(
            db.type_of(&t.write("f/run", "#!/bin/sh\necho hi\n")),
            "application/x-shellscript"
        );
        // The name alone says text/x-python (weight 60), but the content may
        // confirm the lower-weight text/x-python3 glob, as in KDE and GLib.
        assert_eq!(db.type_of_name("a.py").as_deref(), Some("text/x-python"));
        assert_eq!(
            db.type_of(&t.write("f/a.py", "print(1)\n")),
            "text/x-python"
        );
        assert_eq!(
            db.type_of(&t.write("f/b.py", "#!/usr/bin/env python3\nprint(1)\n")),
            "text/x-python3"
        );
        assert_eq!(db.type_of(&t.write("f/c.py", "")), "text/x-python");
    }

    #[test]
    fn special_files_and_fallbacks() {
        let (t, db) = mime_tree();
        fs::create_dir_all(t.path("dir.png")).unwrap();
        assert_eq!(db.type_of(&t.path("dir.png")), DIRECTORY);
        // Empty files: x-zerosize unless the name alone settles it.
        assert_eq!(db.type_of(&t.write("e/empty", b"")), ZERO_SIZE);
        assert_eq!(db.type_of(&t.write("e/empty.rs", b"")), "text/x-rust");
        assert_eq!(db.type_of(&t.write("e/empty.amb", b"")), "audio/x-amb");
        // Missing files (or broken symlinks) fall back to the name.
        assert_eq!(db.type_of(&t.path("e/missing.png")), "image/png");
        assert_eq!(db.type_of(&t.path("e/missing")), OCTET_STREAM);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(t.path("dir.png"), t.path("e/link")).unwrap();
            assert_eq!(db.type_of(&t.path("e/link")), DIRECTORY);
            std::os::unix::fs::symlink(t.path("nowhere"), t.path("e/broken.html")).unwrap();
            assert_eq!(db.type_of(&t.path("e/broken.html")), "text/html");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_falls_back_to_name() {
        use std::os::unix::fs::PermissionsExt;
        let (t, db) = mime_tree();
        let p = t.write("u/x.amb", b"AMBV");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&p).is_ok() {
            return; // running as root: permissions do not apply
        }
        assert_eq!(db.type_of(&p), "audio/x-amb");
        let q = t.write("u/noname", b"\x89PNG");
        fs::set_permissions(&q, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(db.type_of(&q), OCTET_STREAM);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_not_opened() {
        let (t, db) = mime_tree();
        let fifo = t.path("pipe.png");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if made.is_ok_and(|s| s.success()) {
            // Opening it would block forever; type_of must not.
            assert_eq!(db.type_of(&fifo), "inode/fifo");
        }
    }

    #[test]
    fn no_magic_in_higher_dir() {
        let (t, _) = mime_tree();
        let mut home = b"MIME-Magic\0\n[50:image/png]\n>0=__NOMAGIC__\n".to_vec();
        home.extend(rule(0, 0, b"PNGX", None, 1, 1));
        t.write("home/mime/magic", home);
        let db = MimeDb::load(&[t.path("home"), t.path("share")]);
        assert_eq!(
            db.type_of(&t.write("f/a", b"\x89PNG\x00\x01")),
            OCTET_STREAM
        );
        assert_eq!(db.type_of(&t.write("f/b", b"PNGX")), "image/png");
    }

    #[test]
    fn magic_parser_skips_unknown_extensions() {
        let t = Tree::new();
        let mut data = b"MIME-Magic\0\n[50:image/x-a]\n".to_vec();
        // A future extension after the rule: the whole line is ignored...
        data.extend(b">0=\x00\x02AA!future stuff\n");
        // ...and parsing resumes on the next line.
        data.extend(rule(0, 0, b"BB", None, 1, 1));
        t.write("share/mime/magic", data);
        let db = MimeDb::load(&[t.path("share")]);
        assert_eq!(db.type_of(&t.write("f/a", b"AA\x00\x01")), OCTET_STREAM);
        assert_eq!(db.type_of(&t.write("f/b", b"BB\x00\x01")), "image/x-a");
    }

    #[test]
    fn aliases_and_subclasses() {
        let (_t, db) = mime_tree();
        assert!(
            db.is_a("text/x-rust", TEXT_PLAIN),
            "text/* is implicitly text/plain"
        );
        assert!(db.is_a("text/x-rust", OCTET_STREAM));
        assert!(db.is_a("text/rust", "text/x-rust"), "alias resolves");
        assert!(db.is_a("application/x-pdf", "application/pdf"));
        assert!(
            db.is_a("image/svg+xml", TEXT_PLAIN),
            "through application/xml"
        );
        assert!(db.is_a("image/png", OCTET_STREAM));
        assert!(!db.is_a("image/png", TEXT_PLAIN));
        assert!(
            !db.is_a(DIRECTORY, OCTET_STREAM),
            "inode/* is not streamable"
        );
        assert!(!db.is_a(TEXT_PLAIN, "text/x-rust"));
        assert_eq!(
            db.ancestors("image/svg+xml"),
            ["image/svg+xml", "application/xml", TEXT_PLAIN, OCTET_STREAM]
        );
        assert_eq!(db.ancestors(OCTET_STREAM), [OCTET_STREAM]);
        assert_eq!(db.unalias("application/x-pdf"), "application/pdf");
    }

    fn test_env(t: &Tree, locale: Option<&str>, terminal: Option<&str>) -> Env {
        Env::new(
            vec![t.path("bin")],
            locale,
            terminal.map(String::from),
            Some(t.path("home")),
        )
    }

    fn desktop(name: &str, exec: &str, mime: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType={mime}\n{extra}\n")
    }

    /// Three data dirs (home, local, system) and two config dirs.
    struct AppsTree {
        t: Tree,
    }

    impl AppsTree {
        fn new() -> AppsTree {
            let t = Tree::new();
            for prog in [
                "kate",
                "gedit",
                "zed",
                "okteta",
                "feh",
                "gimp",
                "htop",
                "konsole",
                "alacritty",
                "kitty",
                "xterm",
            ] {
                #[cfg(unix)]
                t.write_exec(&format!("bin/{prog}"), "#!/bin/sh\n");
            }
            AppsTree { t }
        }
        fn config(&self) -> Vec<PathBuf> {
            vec![self.t.path("config"), self.t.path("etc-xdg")]
        }
        fn data(&self) -> Vec<PathBuf> {
            vec![
                self.t.path("data"),
                self.t.path("local-share"),
                self.t.path("share"),
            ]
        }
        fn load(&self, desktop: Option<&str>) -> Apps {
            Apps::load_with(
                &self.config(),
                &self.data(),
                desktop,
                &test_env(&self.t, Some("fr_FR.UTF-8"), None),
            )
        }
    }

    fn ids(list: &[&App]) -> Vec<String> {
        list.iter().map(|a| a.id.clone()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn desktop_entries_are_parsed_and_filtered() {
        let a = AppsTree::new();
        let t = &a.t;
        t.write(
            "share/applications/org.kde.kate.desktop",
            "# leading comment\n[Desktop Entry]\nType=Application\nName=Kate\nName[fr]=Kate FR\nName[fr_FR]=Kate France\n\
             Name[de]=Kate DE\nExec = kate -b %U\nIcon=kate\nMimeType=text/plain;text/x-rust\\;odd;\nTerminal=false\n\
             [Desktop Action new]\nName=Ignored\nExec=ignored\n",
        );
        t.write(
            "share/applications/htop.desktop",
            desktop("Htop", "htop", "", "Terminal=true\nNoDisplay=true"),
        );
        t.write(
            "share/applications/hidden.desktop",
            desktop("Hidden", "kate", "text/plain;", "Hidden=true"),
        );
        t.write(
            "share/applications/link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=https://x\n",
        );
        t.write(
            "share/applications/tryexec.desktop",
            desktop("T", "kate", "text/plain;", "TryExec=not-installed"),
        );
        t.write(
            "share/applications/noprog.desktop",
            desktop("N", "/nonexistent/prog %f", "text/plain;", ""),
        );
        t.write(
            "share/applications/badcode.desktop",
            desktop("B", "kate %z", "text/plain;", ""),
        );
        t.write(
            "share/applications/noexec.desktop",
            "[Desktop Entry]\nType=Application\nName=X\n",
        );
        t.write(
            "share/applications/escaped.desktop",
            desktop("Line\\sOne", "kate", "", "Icon=/icons/a\\sb.png"),
        );
        let apps = a.load(None);
        let mut all: Vec<&str> = apps.all().iter().map(|a| a.id.as_str()).collect();
        all.sort();
        assert_eq!(
            all,
            ["escaped.desktop", "htop.desktop", "org.kde.kate.desktop"]
        );
        let kate = apps.get("org.kde.kate.desktop").unwrap();
        assert_eq!(kate.name, "Kate France");
        assert_eq!(kate.exec, "kate -b %U");
        assert_eq!(kate.icon.as_deref(), Some("kate"));
        assert_eq!(kate.mime_types, ["text/plain", "text/x-rust;odd"]);
        assert!(!kate.terminal);
        let htop = apps.get("htop.desktop").unwrap();
        assert!(htop.terminal && htop.no_display);
        let esc = apps.get("escaped.desktop").unwrap();
        assert_eq!(esc.name, "Line One");
        assert_eq!(esc.icon.as_deref(), Some("/icons/a b.png"));

        // Locale fallbacks: lang only, lang@modifier, C.
        let load = |loc: Option<&str>| {
            Apps::load_with(&a.config(), &a.data(), None, &test_env(t, loc, None))
                .get("org.kde.kate.desktop")
                .unwrap()
                .name
                .clone()
        };
        assert_eq!(load(Some("fr_BE.UTF-8")), "Kate FR");
        assert_eq!(load(Some("de_DE@euro")), "Kate DE");
        assert_eq!(load(Some("C.UTF-8")), "Kate");
        assert_eq!(load(None), "Kate");
    }

    #[test]
    fn locale_variant_order() {
        assert_eq!(
            locale_variants(Some("sr_RS.UTF-8@latin")),
            ["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
        assert_eq!(locale_variants(Some("fr_FR")), ["fr_FR", "fr"]);
        assert_eq!(locale_variants(Some("fr")), ["fr"]);
        assert!(locale_variants(Some("POSIX")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn shadowing_and_subdirectory_ids() {
        let a = AppsTree::new();
        let t = &a.t;
        t.write(
            "data/applications/kate.desktop",
            desktop("Kate (mine)", "kate", "text/plain;", ""),
        );
        t.write(
            "share/applications/kate.desktop",
            desktop("Kate (system)", "kate", "text/plain;", ""),
        );
        t.write(
            "data/applications/gedit.desktop",
            desktop("Gedit", "gedit", "text/plain;", "Hidden=true"),
        );
        t.write(
            "share/applications/gedit.desktop",
            desktop("Gedit", "gedit", "text/plain;", ""),
        );
        t.write(
            "share/applications/kde/okteta.desktop",
            desktop("Okteta", "okteta", "application/octet-stream;", ""),
        );
        t.write(
            "local-share/applications/kde-okteta.desktop",
            desktop("Okteta local", "okteta", "", ""),
        );
        let apps = a.load(None);
        assert_eq!(apps.get("kate.desktop").unwrap().name, "Kate (mine)");
        assert!(
            apps.get("gedit.desktop").is_none(),
            "Hidden in home deletes the system entry"
        );
        // kde/okteta.desktop has ID kde-okteta.desktop, shadowed by the
        // higher-precedence local-share file with that ID.
        assert_eq!(apps.get("kde-okteta.desktop").unwrap().name, "Okteta local");
        assert_eq!(apps.all().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn mimeapps_default_precedence() {
        let a = AppsTree::new();
        let t = &a.t;
        for (id, prog) in [
            ("kate", "kate"),
            ("gedit", "gedit"),
            ("zed", "zed"),
            ("okteta", "okteta"),
        ] {
            t.write(
                &format!("share/applications/{id}.desktop"),
                desktop(id, prog, "text/plain;", ""),
            );
        }
        t.write(
            "share/applications/mimeapps.list",
            "[Default Applications]\ntext/plain=gedit.desktop\n",
        );
        let db = MimeDb::default();
        let def = |apps: &Apps| apps.default_for("text/plain", &db).map(|a| a.id.clone());
        assert_eq!(def(&a.load(None)).as_deref(), Some("gedit.desktop"));

        // The data home beats the system data dirs, config dirs beat both.
        t.write(
            "data/applications/mimeapps.list",
            "[Default Applications]\ntext/plain=zed.desktop\n",
        );
        assert_eq!(def(&a.load(None)).as_deref(), Some("zed.desktop"));
        t.write(
            "etc-xdg/mimeapps.list",
            "[Default Applications]\ntext/plain=okteta.desktop\n",
        );
        assert_eq!(def(&a.load(None)).as_deref(), Some("okteta.desktop"));
        // An uninstalled first choice falls through to the next in the list.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\ntext/plain=gone.desktop;kate.desktop;\n",
        );
        assert_eq!(def(&a.load(None)).as_deref(), Some("kate.desktop"));
        // A list with nothing installed falls through to the next file.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\ntext/plain=gone.desktop\n",
        );
        assert_eq!(def(&a.load(None)).as_deref(), Some("okteta.desktop"));

        // Desktop-specific files: $desktop is XDG_CURRENT_DESKTOP lowercased,
        // so KDE reads kde-mimeapps.list, before mimeapps.list of the same dir.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\ntext/plain=kate.desktop\n",
        );
        t.write(
            "config/kde-mimeapps.list",
            "[Default Applications]\ntext/plain=zed.desktop\n",
        );
        assert_eq!(def(&a.load(Some("KDE"))).as_deref(), Some("zed.desktop"));
        assert_eq!(def(&a.load(None)).as_deref(), Some("kate.desktop"));
        assert_eq!(def(&a.load(Some("GNOME"))).as_deref(), Some("kate.desktop"));
        // Each name of a colon-separated XDG_CURRENT_DESKTOP, in order.
        t.write(
            "config/gnome-mimeapps.list",
            "[Default Applications]\ntext/plain=gedit.desktop\n",
        );
        assert_eq!(
            def(&a.load(Some("GNOME:KDE"))).as_deref(),
            Some("gedit.desktop")
        );
        assert_eq!(
            def(&a.load(Some("KDE:GNOME"))).as_deref(),
            Some("zed.desktop")
        );
        // A desktop-specific file in a lower dir loses to a plain file in a higher one.
        fs::remove_file(t.path("config/kde-mimeapps.list")).unwrap();
        t.write(
            "etc-xdg/kde-mimeapps.list",
            "[Default Applications]\ntext/plain=zed.desktop\n",
        );
        assert_eq!(def(&a.load(Some("KDE"))).as_deref(), Some("kate.desktop"));
        // Desktop-specific files cannot add or remove associations.
        t.write(
            "config/kde-mimeapps.list",
            "[Removed Associations]\ntext/plain=kate.desktop\n[Added Associations]\nimage/png=kate.desktop\n",
        );
        let apps = a.load(Some("KDE"));
        assert_eq!(def(&apps).as_deref(), Some("kate.desktop"));
        assert!(apps.for_type("image/png", &db).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn associations_added_removed_and_subclass_fallback() {
        let a = AppsTree::new();
        let t = &a.t;
        let (_m, db) = mime_tree();
        t.write(
            "share/applications/kate.desktop",
            desktop("Kate", "kate %U", "text/plain;", ""),
        );
        t.write(
            "share/applications/gedit.desktop",
            desktop("Gedit", "gedit %U", "text/plain;", ""),
        );
        t.write(
            "share/applications/zed.desktop",
            desktop("Zed", "zed %F", "text/x-rust;", ""),
        );
        t.write(
            "share/applications/okteta.desktop",
            desktop("Okteta", "okteta %U", "application/octet-stream;", ""),
        );
        t.write(
            "share/applications/feh.desktop",
            desktop("feh", "feh %F", "image/png;", ""),
        );
        t.write(
            "share/applications/gimp.desktop",
            desktop("GIMP", "gimp %U", "image/png;", ""),
        );

        let apps = a.load(None);
        // The type's own handler beats its parents' (spec: "an application
        // that can handle a more specific type will be used in preference").
        assert_eq!(
            apps.default_for("text/x-rust", &db).unwrap().id,
            "zed.desktop"
        );
        assert_eq!(
            ids(&apps.for_type("text/x-rust", &db)),
            [
                "zed.desktop",
                "gedit.desktop",
                "kate.desktop",
                "okteta.desktop"
            ]
        );
        // Aliases resolve.
        assert_eq!(
            apps.default_for("text/rust", &db).unwrap().id,
            "zed.desktop"
        );
        // Nothing for the type itself: fall back through the subclass chain.
        assert_eq!(
            apps.default_for("text/x-csrc", &db).unwrap().id,
            "gedit.desktop"
        );
        assert_eq!(
            apps.default_for("application/pdf", &db).unwrap().id,
            "okteta.desktop"
        );
        assert!(apps.default_for(DIRECTORY, &db).is_none());

        // A default for the parent type is used when the child has no
        // default of its own... but only after the child's own handlers.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\ntext/plain=kate.desktop\n",
        );
        let apps = a.load(None);
        assert_eq!(
            apps.default_for("text/x-csrc", &db).unwrap().id,
            "kate.desktop"
        );
        assert_eq!(
            apps.default_for("text/x-rust", &db).unwrap().id,
            "zed.desktop"
        );

        // Added Associations come first, in their order; Removed ones vanish;
        // the default leads; nothing is listed twice.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\nimage/png=gimp.desktop\n\
             [Added Associations]\nimage/png=kate.desktop;feh.desktop;\ntext/x-rust=okteta.desktop;\n\
             [Removed Associations]\ntext/x-rust=gedit.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(
            ids(&apps.for_type("image/png", &db)),
            [
                "gimp.desktop",
                "kate.desktop",
                "feh.desktop",
                "okteta.desktop"
            ]
        );
        // The user's Added Association outranks the system desktop file.
        // gedit is removed for text/x-rust and stays out even though it
        // handles text/plain.
        assert_eq!(
            apps.default_for("text/x-rust", &db).unwrap().id,
            "okteta.desktop"
        );
        assert_eq!(
            ids(&apps.for_type("text/x-rust", &db)),
            ["okteta.desktop", "zed.desktop", "kate.desktop"]
        );
        assert_eq!(
            ids(&apps.for_type("text/plain", &db)),
            ["gedit.desktop", "kate.desktop", "okteta.desktop"]
        );

        // Removing every handler of the type makes the default fall back to
        // the parent, skipping removed ones.
        t.write(
            "config/mimeapps.list",
            "[Removed Associations]\ntext/x-rust=zed.desktop;gedit.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(
            apps.default_for("text/x-rust", &db).unwrap().id,
            "kate.desktop"
        );

        // A Removed default is not a default.
        t.write(
            "config/mimeapps.list",
            "[Default Applications]\nimage/png=gimp.desktop\n[Removed Associations]\nimage/png=gimp.desktop\n",
        );
        let apps = a.load(None);
        assert_eq!(
            apps.default_for("image/png", &db).unwrap().id,
            "feh.desktop"
        );
        assert_eq!(
            ids(&apps.for_type("image/png", &db)),
            ["feh.desktop", "okteta.desktop"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn associations_respect_directory_levels() {
        let a = AppsTree::new();
        let t = &a.t;
        let db = MimeDb::default();
        // kate lives in the data home; zed and gedit in the system dir.
        t.write(
            "data/applications/kate.desktop",
            desktop("Kate", "kate", "text/plain;", ""),
        );
        t.write(
            "share/applications/zed.desktop",
            desktop("Zed", "zed", "", ""),
        );
        t.write(
            "share/applications/gedit.desktop",
            desktop("Gedit", "gedit", "text/plain;", ""),
        );
        // A system mimeapps.list cannot add associations to a desktop file
        // that a higher directory overrides...
        t.write(
            "share/applications/mimeapps.list",
            "[Added Associations]\nimage/png=kate.desktop;zed.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(ids(&apps.for_type("image/png", &db)), ["zed.desktop"]);
        // ...nor remove theirs.
        t.write(
            "share/applications/mimeapps.list",
            "[Removed Associations]\ntext/plain=kate.desktop;gedit.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(ids(&apps.for_type("text/plain", &db)), ["kate.desktop"]);
        // A removal in the user config affects every directory below it.
        t.write(
            "config/mimeapps.list",
            "[Removed Associations]\ntext/plain=kate.desktop;\n",
        );
        t.write("share/applications/mimeapps.list", "");
        let apps = a.load(None);
        assert_eq!(ids(&apps.for_type("text/plain", &db)), ["gedit.desktop"]);
        // An addition in the same file as a removal for another dir still works,
        // and a lower-level addition of a removed app is blocked.
        t.write(
            "config/mimeapps.list",
            "[Added Associations]\nimage/png=gedit.desktop;\n[Removed Associations]\nimage/png=zed.desktop;\n",
        );
        t.write(
            "share/applications/mimeapps.list",
            "[Added Associations]\nimage/png=zed.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(ids(&apps.for_type("image/png", &db)), ["gedit.desktop"]);
        // A distribution removal is undone by a user addition for the same type.
        t.write(
            "share/applications/mimeapps.list",
            "[Removed Associations]\ntext/plain=gedit.desktop;\n",
        );
        t.write(
            "config/mimeapps.list",
            "[Added Associations]\ntext/plain=gedit.desktop;\n",
        );
        let apps = a.load(None);
        assert_eq!(
            ids(&apps.for_type("text/plain", &db)),
            ["gedit.desktop", "kate.desktop"]
        );
        assert_eq!(
            apps.default_for("text/plain", &db).unwrap().id,
            "gedit.desktop"
        );
    }

    fn app(exec: &str) -> App {
        App {
            id: "test.desktop".into(),
            name: "Test App".into(),
            icon: Some("test-icon".into()),
            exec: exec.into(),
            terminal: false,
            path: PathBuf::from("/usr/share/applications/test.desktop"),
            mime_types: Vec::new(),
            no_display: false,
        }
    }

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn field_codes() {
        let files = [PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/ü%#.txt")];
        assert_eq!(
            app("prog %f").commands(&files),
            [sv(&["prog", "/tmp/a b.txt"]), sv(&["prog", "/tmp/ü%#.txt"])]
        );
        assert_eq!(
            app("prog %F").commands(&files),
            [sv(&["prog", "/tmp/a b.txt", "/tmp/ü%#.txt"])]
        );
        assert_eq!(
            app("prog %U").commands(&files),
            [sv(&[
                "prog",
                "file:///tmp/a%20b.txt",
                "file:///tmp/%C3%BC%25%23.txt"
            ])]
        );
        assert_eq!(
            app("prog %u").commands(&files),
            [
                sv(&["prog", "file:///tmp/a%20b.txt"]),
                sv(&["prog", "file:///tmp/%C3%BC%25%23.txt"])
            ]
        );
        // %i, %c, %k; %% is a literal percent; deprecated codes vanish.
        assert_eq!(
            app("prog %i --name %c --from %k 100%% %d %D %n %N %v %m x%dy %F")
                .commands(&files[..1]),
            [sv(&[
                "prog",
                "--icon",
                "test-icon",
                "--name",
                "Test App",
                "--from",
                "/usr/share/applications/test.desktop",
                "100%",
                "xy",
                "/tmp/a b.txt",
            ])]
        );
        let mut no_icon = app("prog %i %f");
        no_icon.icon = None;
        assert_eq!(
            no_icon.commands(&files[..1]),
            [sv(&["prog", "/tmp/a b.txt"])]
        );
        // No files: the file codes disappear, the app just starts.
        assert_eq!(app("prog %U --new").commands(&[]), [sv(&["prog", "--new"])]);
        assert_eq!(app("prog %f").commands(&[]), [sv(&["prog"])]);
        // No field code at all: the file is appended, one run per file.
        assert_eq!(
            app("prog --open").commands(&files),
            [
                sv(&["prog", "--open", "/tmp/a b.txt"]),
                sv(&["prog", "--open", "/tmp/ü%#.txt"])
            ]
        );
        // A code inside an argument stays one argument.
        assert_eq!(
            app("prog --file=%f").commands(&files[..1]),
            [sv(&["prog", "--file=/tmp/a b.txt"])]
        );
        // Relative paths are made absolute (a file name alone means nothing
        // to a program started elsewhere).
        let rel = app("prog %f").commands(&[PathBuf::from("rel.txt")]);
        assert!(Path::new(&rel[0][1]).is_absolute() && rel[0][1].ends_with("rel.txt"));
        // Invalid lines produce nothing rather than something wrong.
        assert!(app("prog %z").commands(&files).is_empty());
        assert!(app("prog \"unterminated").commands(&files).is_empty());
    }

    #[test]
    fn exec_quoting() {
        // Quoted program path with spaces; escapes inside double quotes.
        assert_eq!(
            app(r#""/opt/My App/bin/app" --title "a \"b\" \$HOME \`x\` \\ c" %f"#)
                .commands(&[PathBuf::from("/f")]),
            [sv(&[
                "/opt/My App/bin/app",
                "--title",
                r#"a "b" $HOME `x` \ c"#,
                "/f"
            ])]
        );
        // Empty quoted arguments survive; runs of spaces do not create any.
        assert_eq!(
            app("prog  \"\"   x").commands(&[]),
            [sv(&["prog", "", "x"])]
        );
        // Field codes inside quotes are quoted for the shell (sh -c "...").
        assert_eq!(
            app(r#"sh -c "grep foo %f | less""#).commands(&[PathBuf::from("/tmp/it's here")]),
            [sv(&["sh", "-c", r"grep foo '/tmp/it'\''s here' | less"])]
        );
        // Single quotes, as tolerated by GLib and KDE.
        assert_eq!(
            app("sh -c 'echo $0' %f").commands(&[PathBuf::from("/x")]),
            [sv(&["sh", "-c", "echo $0", "/x"])]
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_string_escapes_then_quoting_from_a_file() {
        // In the file, a literal backslash inside a quoted argument takes
        // four backslashes and a dollar takes \\$ (string escapes are undone
        // first, then the quoting).
        let a = AppsTree::new();
        a.t.write(
            "share/applications/q.desktop",
            "[Desktop Entry]\nType=Application\nName=Q\nExec=kate \"a\\\\\\\\b\" \"\\\\$PATH\" x\\sy %f\n",
        );
        let apps = a.load(None);
        let q = apps.get("q.desktop").unwrap();
        assert_eq!(
            q.commands(&[PathBuf::from("/f")]),
            [sv(&["kate", r"a\b", "$PATH", "x", "y", "/f"])]
        );
    }

    #[test]
    fn file_uris() {
        assert_eq!(file_uri(Path::new("/a b/c?d#e")), "file:///a%20b/c%3Fd%23e");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = Path::new(std::ffi::OsStr::from_bytes(b"/x/\xff.bin"));
            assert_eq!(file_uri(raw), "file:///x/%FF.bin");
        }
    }

    #[cfg(unix)]
    #[test]
    fn custom_commands() {
        let a = AppsTree::new();
        let env = test_env(&a.t, None, None);
        let files = [PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")];
        let run = |line: &str, files: &[PathBuf]| custom_command_with(line, files, &env);
        assert_eq!(
            run("kate -n", &files).unwrap(),
            sv(&["kate", "-n", "/tmp/a b", "/tmp/c"])
        );
        assert_eq!(
            run("kate --x='a b' %F --y", &files).unwrap(),
            sv(&["kate", "--x=a b", "/tmp/a b", "/tmp/c", "--y"])
        );
        // A standalone %f takes all files: only one command can come back.
        assert_eq!(
            run("kate %f", &files).unwrap(),
            sv(&["kate", "/tmp/a b", "/tmp/c"])
        );
        assert_eq!(
            run("kate %U", &files[1..]).unwrap(),
            sv(&["kate", "file:///tmp/c"])
        );
        assert_eq!(
            run("kate --file=%f", &files[1..]).unwrap(),
            sv(&["kate", "--file=/tmp/c"])
        );
        assert!(run("kate --file=%f", &files).is_err());
        // Other percent signs are left alone.
        assert_eq!(
            run(r#"kate +%Y "x\"y""#, &[]).unwrap(),
            sv(&["kate", "+%Y", "x\"y"])
        );
        assert!(run("", &files).is_err());
        assert!(run("   ", &files).is_err());
        assert!(run("kate 'open", &files).is_err());
        assert_eq!(
            run("no-such-program", &files).unwrap_err(),
            "no-such-program: command not found"
        );
        let abs = a.t.path("bin/kate");
        assert!(run(&format!("{} %f", abs.display()), &files).is_ok());
    }

    /// A minimal ELF image: header, program headers, optional dynamic section.
    fn elf(e_type: u16, phdrs: &[(u32, u64, u64)], dynamic: &[(u64, u64)]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2; // 64-bit
        b[5] = 1; // little-endian
        b[6] = 1;
        b[16..18].copy_from_slice(&e_type.to_le_bytes());
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
        for &(p_type, offset, size) in phdrs {
            let mut ph = vec![0u8; 56];
            ph[..4].copy_from_slice(&p_type.to_le_bytes());
            ph[8..16].copy_from_slice(&offset.to_le_bytes());
            ph[32..40].copy_from_slice(&size.to_le_bytes());
            b.extend(ph);
        }
        for &(tag, val) in dynamic {
            b.extend(tag.to_le_bytes());
            b.extend(val.to_le_bytes());
        }
        b
    }

    #[cfg(unix)]
    #[test]
    fn executable_check() {
        use std::os::unix::fs::PermissionsExt;
        let (t, db) = mime_tree();
        let exec_bit = |p: &Path, on: bool| {
            fs::set_permissions(
                p,
                fs::Permissions::from_mode(if on { 0o755 } else { 0o644 }),
            )
            .unwrap()
        };
        let script = t.write("x/run.sh", "#!/bin/sh\necho hi\n");
        exec_bit(&script, true);
        assert!(is_executable(&script, "application/x-shellscript", &db));
        assert!(is_executable(&script, TEXT_PLAIN, &db));
        exec_bit(&script, false);
        assert!(!is_executable(&script, "application/x-shellscript", &db));
        // A JPEG with the x bit (every file on a FAT stick has it).
        let jpeg = t.write("x/photo.jpg", b"\xff\xd8\xff\xe0\x00\x10JFIF");
        exec_bit(&jpeg, true);
        assert!(!is_executable(&jpeg, "image/jpeg", &db));
        // Text with the x bit but no shebang.
        let notes = t.write("x/notes.txt", "just notes\n");
        exec_bit(&notes, true);
        assert!(!is_executable(&notes, TEXT_PLAIN, &db));
        // Directories have x bits too.
        assert!(!is_executable(&t.path("x"), DIRECTORY, &db));

        // ELF: ET_EXEC, PIE (ET_DYN + PT_INTERP), static-pie (DF_1_PIE),
        // and a plain shared library, which must not be "run".
        let interp_at = 64 + 2 * 56;
        let cases: [(&str, Vec<u8>, bool); 4] = [
            ("exec", elf(2, &[], &[]), true),
            ("pie", elf(3, &[(1, 0, 0), (3, interp_at, 16)], &[]), true),
            (
                "static-pie",
                elf(
                    3,
                    &[(2, 64 + 56, 32)],
                    &[(0x6fff_fffb, 0x0800_0001), (0, 0)],
                ),
                true,
            ),
            (
                "lib.so.1",
                elf(3, &[(1, 0, 0), (2, 64 + 2 * 56, 32)], &[(1, 5), (0, 0)]),
                false,
            ),
        ];
        for (name, bytes, expected) in cases {
            let p = t.write(&format!("x/{name}"), &bytes);
            exec_bit(&p, true);
            assert_eq!(
                is_executable(&p, "application/x-sharedlib", &db),
                expected,
                "{name}"
            );
            assert_eq!(
                is_executable(&p, OCTET_STREAM, &db),
                expected,
                "{name} as octet-stream"
            );
            exec_bit(&p, false);
            assert!(
                !is_executable(&p, "application/x-sharedlib", &db),
                "{name} without x"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn terminal_selection() {
        let a = AppsTree::new();
        let t = &a.t;
        let find =
            |terminal: Option<&str>| find_terminal_with(&a.config(), &test_env(t, None, terminal));
        // konsole is the fallback (no $TERMINAL, no kdeglobals).
        assert_eq!(find(None).unwrap(), sv(&["konsole", "-e"]));
        // kdeglobals [General] TerminalApplication, user file first.
        t.write(
            "etc-xdg/kdeglobals",
            "[General]\nTerminalApplication=xterm\n",
        );
        assert_eq!(find(None).unwrap(), sv(&["xterm", "-e"]));
        t.write(
            "config/kdeglobals",
            "[KDE]\nTerminalApplication=wrong-group\n[General]\nTerminalApplication=kitty\n",
        );
        assert_eq!(find(None).unwrap(), sv(&["kitty"]));
        // An uninstalled choice is skipped. Like KConfig, the user's value
        // overrides the system one even then, so konsole comes next.
        t.write(
            "config/kdeglobals",
            "[General]\nTerminalApplication[$e]=not-installed --x\n",
        );
        assert_eq!(find(None).unwrap(), sv(&["konsole", "-e"]));
        // $TERMINAL wins, with its arguments, without doubling the flag.
        assert_eq!(
            find(Some("alacritty -e")).unwrap(),
            sv(&["alacritty", "-e"])
        );
        assert_eq!(
            find(Some("alacritty --class x")).unwrap(),
            sv(&["alacritty", "--class", "x", "-e"])
        );
        // Nothing installed at all.
        let empty = Env::new(vec![], None, None, None);
        assert_eq!(find_terminal_with(&[], &empty), None);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_apps_are_wrapped() {
        let a = AppsTree::new();
        a.t.write(
            "share/applications/htop.desktop",
            desktop("Htop", "htop %f", "", "Terminal=true"),
        );
        a.t.write(
            "share/applications/feh.desktop",
            desktop("feh", "feh %F", "", ""),
        );
        let apps = a.load(None);
        let files = [PathBuf::from("/x")];
        let htop = apps.get("htop.desktop").unwrap();
        assert_eq!(
            apps.launch_commands(htop, &files).unwrap(),
            [sv(&["konsole", "-e", "htop", "/x"])]
        );
        let feh = apps.get("feh.desktop").unwrap();
        assert_eq!(
            apps.launch_commands(feh, &files).unwrap(),
            [sv(&["feh", "/x"])]
        );
        let mut no_terminal = apps.clone();
        no_terminal.terminal = None;
        assert!(no_terminal.launch_commands(htop, &files).is_err());
    }

    #[test]
    fn empty_world_is_harmless() {
        let t = Tree::new();
        let db = MimeDb::load(&[t.path("nothing")]);
        let apps = Apps::load(&[t.path("nothing")], &[t.path("nothing")], Some("KDE"));
        assert_eq!(db.type_of_name("a.png"), None);
        assert_eq!(db.type_of(&t.write("a.txt", "hi")), TEXT_PLAIN);
        assert!(apps.all().is_empty());
        assert!(apps.default_for("text/plain", &db).is_none());
        assert!(apps.for_type("text/plain", &db).is_empty());
    }

    #[test]
    fn databases_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<MimeDb>();
        check::<Apps>();
    }
}
