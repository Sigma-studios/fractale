//! The shared MIME database (shared-mime-info), and the XDG base
//! directories it's read from. The system thumbnailers find which files
//! they take with it, and Fractale's "what opens this" uses it too.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Upper bound on how much of a file is read for content sniffing. Magic
/// rules almost all look at the first few hundred bytes; GLib uses the same
/// 4 KiB cap, and reading one page costs the same as reading less.
pub const SNIFF_LEN: usize = 4096;

pub const OCTET_STREAM: &str = "application/octet-stream";
pub const TEXT_PLAIN: &str = "text/plain";
pub const ZERO_SIZE: &str = "application/x-zerosize";
pub const DIRECTORY: &str = "inode/directory";

// ---------------------------------------------------------------------------
// XDG base directories
// ---------------------------------------------------------------------------

pub fn env_nonempty(var: &str) -> Option<String> {
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
pub fn dedup_paths(dirs: &[PathBuf]) -> Vec<PathBuf> {
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
    pub fn names_of<'a>(&'a self, canonical: &'a str) -> Vec<&'a str> {
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

pub fn read_head(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
