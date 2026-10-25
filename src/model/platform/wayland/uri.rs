//! Pure helpers: `text/uri-list` encoding/decoding and clipboard payloads.
//! No Wayland here, so it is unit-tested on any Unix.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::Clip;

pub const URI_LIST: &str = "text/uri-list";
pub const TEXT_UTF8: &str = "text/plain;charset=utf-8";
pub const KDE_CUT: &str = "application/x-kde-cutselection";
pub const GNOME_FILES: &str = "x-special/gnome-copied-files";

/// Text MIME types we offer, in order of preference when reading.
pub const TEXT_TYPES: [&str; 5] = [TEXT_UTF8, "UTF8_STRING", "text/plain", "TEXT", "STRING"];

/// A clipboard or drag payload: (MIME type, bytes), in offer order.
pub type Payload = Vec<(String, Vec<u8>)>;

/// `file://` URI for an absolute path, percent-encoding everything except
/// RFC 3986 unreserved characters and `/`.
pub fn path_to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// RFC 2483 list: one URI per line, CRLF-terminated.
pub fn encode_uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| path_to_uri(p) + "\r\n").collect()
}

/// Local paths from a `text/uri-list` (or newline-separated URIs). Non-`file:`
/// URIs, comments and URIs naming another host are skipped.
pub fn decode_uri_list(data: &[u8]) -> Vec<PathBuf> {
    data.split(|&b| b == b'\n')
        .map(trim_bytes)
        .filter(|line| !line.is_empty() && line[0] != b'#')
        .filter_map(uri_to_path)
        .collect()
}

fn trim_bytes(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() || *first == 0 { s = rest } else { break }
    }
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() || *last == 0 { s = rest } else { break }
    }
    s
}

/// `file:///a`, `file://localhost/a`, `file://<this host>/a`, `file:/a`.
pub fn uri_to_path(uri: &[u8]) -> Option<PathBuf> {
    let rest = strip_prefix_ignore_case(uri, b"file:")?;
    let path = if let Some(after) = rest.strip_prefix(b"//") {
        let slash = after.iter().position(|&b| b == b'/')?;
        let host = &after[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case(b"localhost") || is_this_host(host)) {
            return None;
        }
        &after[slash..]
    } else if rest.first() == Some(&b'/') {
        rest
    } else {
        return None;
    };
    // Drop a query or fragment, which a file path can't contain unencoded.
    let end = path.iter().position(|&b| b == b'?' || b == b'#').unwrap_or(path.len());
    let bytes = percent_decode(&path[..end])?;
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn strip_prefix_ignore_case<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn is_this_host(host: &[u8]) -> bool {
    std::fs::read("/proc/sys/kernel/hostname")
        .map(|h| trim_bytes(&h).eq_ignore_ascii_case(host))
        .unwrap_or(false)
}

fn percent_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Paths as plain text, one per line (lossy for non-UTF-8 names).
pub fn paths_to_text(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n")
}

/// Everything we offer for a file clipboard (`cut` adds the KDE/GNOME cut markers).
pub fn files_payload(paths: &[PathBuf], cut: bool) -> Payload {
    let uris = encode_uri_list(paths);
    let text = paths_to_text(paths);
    let gnome = format!(
        "{}\n{}",
        if cut { "cut" } else { "copy" },
        paths.iter().map(|p| path_to_uri(p)).collect::<Vec<_>>().join("\n")
    );
    let mut p: Payload = vec![(URI_LIST.into(), uris.into_bytes())];
    if cut {
        p.push((KDE_CUT.into(), b"1".to_vec()));
    }
    p.push((GNOME_FILES.into(), gnome.into_bytes()));
    for t in [TEXT_UTF8, "UTF8_STRING", "text/plain"] {
        p.push((t.into(), text.clone().into_bytes()));
    }
    p
}

/// Payload of an outgoing drag: no cut marker (the target picks copy/move).
pub fn drag_payload(paths: &[PathBuf]) -> Payload {
    vec![
        (URI_LIST.into(), encode_uri_list(paths).into_bytes()),
        (TEXT_UTF8.into(), paths_to_text(paths).into_bytes()),
    ]
}

pub fn text_payload(text: &str) -> Payload {
    TEXT_TYPES.iter().map(|t| (t.to_string(), text.as_bytes().to_vec())).collect()
}

/// Which MIME types to fetch from a foreign selection offering `mimes`:
/// (uri-list?, cut-marker type?, text type?).
pub fn pick_selection_types(mimes: &[String]) -> (bool, Option<&'static str>, Option<&'static str>) {
    let has = |m: &str| mimes.iter().any(|x| x == m);
    let uri = has(URI_LIST);
    let cut = if !uri {
        None
    } else if has(KDE_CUT) {
        Some(KDE_CUT)
    } else if has(GNOME_FILES) {
        Some(GNOME_FILES)
    } else {
        None
    };
    let text = TEXT_TYPES.iter().copied().find(|t| has(t));
    (uri, cut, text)
}

/// Build the clipboard answer from whatever was read. Files win when the
/// uri-list holds at least one local file; otherwise text; otherwise empty.
pub fn build_clip(uris: Option<&[u8]>, cut_type: Option<&str>, cut: Option<&[u8]>, text: Option<&[u8]>) -> Clip {
    if let Some(paths) = uris.map(decode_uri_list).filter(|p| !p.is_empty()) {
        let cut = match (cut_type, cut) {
            (Some(KDE_CUT), Some(d)) => trim_bytes(d) == b"1",
            (Some(GNOME_FILES), Some(d)) => d.split(|&b| b == b'\n').next().map(trim_bytes) == Some(b"cut"),
            _ => false,
        };
        return Clip::Files { paths, cut };
    }
    match text {
        Some(t) => Clip::Text(String::from_utf8_lossy(t).replace("\r\n", "\n")),
        None => Clip::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip_with_odd_names() {
        let paths = vec![
            PathBuf::from("/tmp/a b/c#d%e.txt"),
            PathBuf::from("/home/u/été/日本"),
            PathBuf::from(OsString::from_vec(b"/tmp/bad\xff\xfename".to_vec())),
        ];
        let list = encode_uri_list(&paths);
        assert!(list.starts_with("file:///tmp/a%20b/c%23d%25e.txt\r\n"));
        assert_eq!(decode_uri_list(list.as_bytes()), paths);
    }

    #[test]
    fn decode_variants() {
        let data = b"# comment\r\nfile:///a/b\r\nfile://localhost/c%20d\nhttps://example.com/x\nfile://otherhost.invalid/e\nfile:/f\n\0";
        assert_eq!(decode_uri_list(data), vec![PathBuf::from("/a/b"), PathBuf::from("/c d"), PathBuf::from("/f")]);
        assert_eq!(uri_to_path(b"FILE:///x"), Some(PathBuf::from("/x")));
        assert_eq!(uri_to_path(b"file:///bad%zz"), None);
        assert_eq!(uri_to_path(b"file:relative"), None);
    }

    #[test]
    fn files_payload_types() {
        let p = files_payload(&[PathBuf::from("/x/y")], true);
        let types: Vec<&str> = p.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(types, [URI_LIST, KDE_CUT, GNOME_FILES, TEXT_UTF8, "UTF8_STRING", "text/plain"]);
        assert_eq!(p[0].1, b"file:///x/y\r\n");
        assert_eq!(p[1].1, b"1");
        assert_eq!(p[2].1, b"cut\nfile:///x/y");
        let copy = files_payload(&[PathBuf::from("/x/y")], false);
        assert!(!copy.iter().any(|(t, _)| t == KDE_CUT));
        assert!(!drag_payload(&[PathBuf::from("/x")]).iter().any(|(t, _)| t == KDE_CUT));
    }

    #[test]
    fn clip_building() {
        let mimes: Vec<String> = [URI_LIST, KDE_CUT, TEXT_UTF8].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_selection_types(&mimes), (true, Some(KDE_CUT), Some(TEXT_UTF8)));
        assert_eq!(
            build_clip(Some(b"file:///a\r\n"), Some(KDE_CUT), Some(b"1"), None),
            Clip::Files { paths: vec![PathBuf::from("/a")], cut: true }
        );
        assert_eq!(
            build_clip(Some(b"file:///a\r\n"), Some(GNOME_FILES), Some(b"copy\nfile:///a"), None),
            Clip::Files { paths: vec![PathBuf::from("/a")], cut: false }
        );
        // A web link: no local files, so fall back to text.
        assert_eq!(
            build_clip(Some(b"https://x.org/\r\n"), None, None, Some(b"https://x.org/")),
            Clip::Text("https://x.org/".into())
        );
        assert_eq!(build_clip(None, None, None, None), Clip::Empty);
        let only_text: Vec<String> = vec!["UTF8_STRING".into()];
        assert_eq!(pick_selection_types(&only_text), (false, None, Some("UTF8_STRING")));
    }
}
