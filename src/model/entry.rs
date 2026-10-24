//! One entry of a folder listing, as `lstat` saw it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    /// Sockets, fifos, devices.
    Other,
}

/// What a symlink points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: PathBuf,
    /// The kind of what it resolves to; `None` when dangling or a loop.
    pub resolves_to: Option<Kind>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub link: Option<Link>,
    /// Apparent size (`st_size`).
    pub size: u64,
    /// Allocated size (`st_blocks × 512`), what répertoire counts.
    pub bytes: u64,
    /// Seconds since the epoch.
    pub mtime: i64,
    pub mode: u32,
    pub ino: u64,
    pub dev: u64,
    pub nlink: u64,
    /// Natural-order sort key (see [`natural_key`]).
    pub key: Box<[u8]>,
    pub category: Category,
}

impl Entry {
    pub fn from_metadata(name: OsString, md: &std::fs::Metadata, dir: &Path) -> Entry {
        let ft = md.file_type();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else if ft.is_symlink() {
            Kind::Symlink
        } else {
            Kind::Other
        };
        let link = (kind == Kind::Symlink).then(|| {
            let path = dir.join(&name);
            let target = std::fs::read_link(&path).unwrap_or_default();
            // metadata() follows the link; loops and dangling links fail.
            let resolves_to = std::fs::metadata(&path).ok().map(|m| if m.is_dir() { Kind::Dir } else if m.is_file() { Kind::File } else { Kind::Other });
            Link { target, resolves_to }
        });
        let category = Category::of(&name, kind, md.mode(), link.as_ref());
        Entry {
            key: natural_key(name.as_bytes()),
            name,
            kind,
            link,
            size: md.size(),
            bytes: md.blocks() * 512,
            mtime: md.mtime(),
            mode: md.mode(),
            ino: md.ino(),
            dev: md.dev(),
            nlink: md.nlink(),
            category,
        }
    }

    pub fn is_hidden(&self) -> bool {
        self.name.as_bytes().first() == Some(&b'.')
    }

    /// Shown in the folder pane: real directories only. Symlinks to
    /// directories go with the files (with a badge) but open as folders.
    pub fn is_folder(&self) -> bool {
        self.kind == Kind::Dir
    }

    /// Opens as a folder (a directory, or a symlink to one).
    pub fn opens_as_folder(&self) -> bool {
        self.kind == Kind::Dir || self.link.as_ref().is_some_and(|l| l.resolves_to == Some(Kind::Dir))
    }

    pub fn name_lossy(&self) -> String {
        self.name.to_string_lossy().into_owned()
    }

    pub fn extension(&self) -> Option<&OsStr> {
        Path::new(&self.name).extension()
    }
}

/// A sort key that orders names the way people expect: case-insensitive,
/// and numbers by value (`file2` before `file10`). Digit runs are encoded as
/// a marker byte, their length without leading zeros, then the digits, so a
/// plain byte comparison of keys is a natural comparison of names. The
/// original name is appended after a separator to break ties (`a` vs `A`,
/// `01` vs `1`) deterministically.
pub fn natural_key(name: &[u8]) -> Box<[u8]> {
    let mut out = Vec::with_capacity(name.len() + 8);
    let mut i = 0;
    while i < name.len() {
        let b = name[i];
        if b.is_ascii_digit() {
            let start = i;
            while i < name.len() && name[i].is_ascii_digit() {
                i += 1;
            }
            let digits = &name[start..i];
            let trimmed = match digits.iter().position(|&d| d != b'0') {
                Some(p) => &digits[p..],
                None => &digits[digits.len() - 1..],
            };
            // 0x01 sorts digits before letters, like most file managers.
            out.push(0x01);
            out.push(trimmed.len().min(255) as u8);
            out.extend_from_slice(trimmed);
            continue;
        }
        out.push(b.to_ascii_lowercase());
        i += 1;
    }
    out.push(0x00);
    out.extend_from_slice(name);
    out.into()
}

/// Broad file types, for icons and grouping. Decided from the name and mode
/// only (no disk access); previews do the real content sniffing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Document,
    Code,
    Executable,
    Other,
}

impl Category {
    pub fn of(name: &OsStr, kind: Kind, mode: u32, link: Option<&Link>) -> Category {
        if kind == Kind::Dir || link.is_some_and(|l| l.resolves_to == Some(Kind::Dir)) {
            return Category::Folder;
        }
        let ext = Path::new(name).extension().map(|e| e.to_ascii_lowercase());
        let ext = ext.as_ref().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "avif" | "heic" | "heif" | "svg" | "ico" | "xcf" | "psd"
            | "kra" | "ora" | "tga" | "dds" | "exr" | "hdr" | "qoi" | "jxl" | "raw" | "cr2" | "nef" | "arw" | "dng" | "pxl" | "prisme" => {
                Category::Image
            }
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ogv" | "m2ts" | "mts" => Category::Video,
            "mp3" | "flac" | "ogg" | "opus" | "wav" | "aac" | "m4a" | "wma" | "aiff" | "aif" | "mid" | "midi" | "orch" => Category::Audio,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "deb" | "rpm" | "iso" | "img" | "dmg" | "jar" | "apk" | "crate" => {
                Category::Archive
            }
            "pdf" | "epub" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "xls" | "xlsx" | "ods" | "csv" | "ppt" | "pptx" | "odp" | "tex" | "djvu" => {
                Category::Document
            }
            "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "swift" | "rb" | "php" | "lua" | "zig"
            | "nix" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" | "json" | "ron" | "xml" | "html" | "css" | "scss" | "sql" | "wgsl"
            | "glsl" | "hlsl" | "vert" | "frag" | "cs" | "lock" | "cmake" | "mk" | "diff" | "patch" | "log" | "ini" | "conf" | "cfg" | "wat" => {
                Category::Code
            }
            _ if kind == Kind::File && mode & 0o111 != 0 => Category::Executable,
            _ => Category::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Folder => "Folder",
            Category::Image => "Image",
            Category::Video => "Video",
            Category::Audio => "Audio",
            Category::Archive => "Archive",
            Category::Document => "Document",
            Category::Code => "Text / code",
            Category::Executable => "Program",
            Category::Other => "File",
        }
    }
}

/// `path` joined with an entry name.
pub fn child(path: &Path, name: &OsStr) -> PathBuf {
    path.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<(Box<[u8]>, &str)> = names.iter().map(|n| (natural_key(n.as_bytes()), *n)).collect();
        v.sort();
        v.into_iter().map(|(_, n)| n.to_string()).collect()
    }

    #[test]
    fn natural_order() {
        assert_eq!(sorted(&["file10", "file2", "file1", "File3", "file02"]), ["file1", "file02", "file2", "File3", "file10"]);
        assert_eq!(sorted(&["b", "A", "a", "B"]), ["A", "a", "B", "b"]);
        assert_eq!(sorted(&["v1.10.0", "v1.9.2", "v1.10.0-rc1"]), ["v1.9.2", "v1.10.0", "v1.10.0-rc1"]);
        assert_eq!(sorted(&["x", "10", "9"]), ["9", "10", "x"]);
        assert_eq!(sorted(&["a0", "a00", "a"]), ["a", "a0", "a00"]);
    }

    #[test]
    fn categories() {
        let c = |n: &str| Category::of(OsStr::new(n), Kind::File, 0o644, None);
        assert_eq!(c("photo.JPG"), Category::Image);
        assert_eq!(c("song.orch"), Category::Audio);
        assert_eq!(c("main.rs"), Category::Code);
        assert_eq!(c("archive.tar.gz"), Category::Archive);
        assert_eq!(c("README"), Category::Other);
        assert_eq!(Category::of(OsStr::new("run"), Kind::File, 0o755, None), Category::Executable);
    }
}
