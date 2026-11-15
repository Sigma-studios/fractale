//! The shared freedesktop thumbnail cache
//! (<https://specifications.freedesktop.org/thumbnail-spec/>):
//! `$XDG_CACHE_HOME/thumbnails/{normal,large,x-large,xx-large}/<md5 of the
//! file URI>.png`, valid while its `Thumb::URI` and `Thumb::MTime` text chunks
//! match the file. Fractale reads what Dolphin and others made, and writes
//! what it makes the same way, so every app benefits.

use md5::{Digest, Md5};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The spec's size buckets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Bucket {
    Normal,
    Large,
    XLarge,
    XXLarge,
}

impl Bucket {
    pub fn pixels(self) -> u32 {
        match self {
            Bucket::Normal => 128,
            Bucket::Large => 256,
            Bucket::XLarge => 512,
            Bucket::XXLarge => 1024,
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Bucket::Normal => "normal",
            Bucket::Large => "large",
            Bucket::XLarge => "x-large",
            Bucket::XXLarge => "xx-large",
        }
    }

    /// The smallest bucket at least `px` physical pixels.
    pub fn for_pixels(px: f32) -> Bucket {
        match px {
            p if p <= 128.0 => Bucket::Normal,
            p if p <= 256.0 => Bucket::Large,
            p if p <= 512.0 => Bucket::XLarge,
            _ => Bucket::XXLarge,
        }
    }
}

/// Where the cache lives.
pub fn root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| "/tmp".into())
        .join("thumbnails")
}

/// The file's URI as the spec wants it: `file://` and the absolute path,
/// percent-encoded except for unreserved characters and `/`.
pub fn uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn name(uri: &str) -> String {
    let digest = Md5::digest(uri.as_bytes());
    let mut s = String::with_capacity(36);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str(".png");
    s
}

pub fn path_in(root: &Path, bucket: Bucket, file: &Path) -> PathBuf {
    root.join(bucket.dir()).join(name(&uri(file)))
}

/// Where failures are recorded, per the spec: `fail/<app>-<version>/`.
pub fn fail_path_in(root: &Path, file: &Path) -> PathBuf {
    root.join("fail").join(format!("fractale-{}", env!("CARGO_PKG_VERSION"))).join(name(&uri(file)))
}

/// Read a cached thumbnail if it's valid for `file` (same URI, same mtime).
pub fn read(root: &Path, bucket: Bucket, file: &Path, mtime: i64) -> Option<image::RgbaImage> {
    let p = path_in(root, bucket, file);
    let data = std::fs::read(&p).ok()?;
    let (img, text) = decode_png(&data)?;
    let want = uri(file);
    let ok_uri = text.iter().any(|(k, v)| k == "Thumb::URI" && *v == want);
    let ok_mtime = text.iter().any(|(k, v)| k == "Thumb::MTime" && v.parse::<i64>().ok() == Some(mtime));
    (ok_uri && ok_mtime).then_some(img)
}

/// Whether a previous attempt for this exact file version failed.
pub fn failed(root: &Path, file: &Path, mtime: i64) -> bool {
    let Ok(data) = std::fs::read(fail_path_in(root, file)) else { return false };
    decode_png(&data).is_some_and(|(_, text)| text.iter().any(|(k, v)| k == "Thumb::MTime" && v.parse::<i64>().ok() == Some(mtime)))
}

fn decode_png(data: &[u8]) -> Option<(image::RgbaImage, Vec<(String, String)>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let info = reader.info();
    let text: Vec<(String, String)> = info
        .uncompressed_latin1_text
        .iter()
        .map(|t| (t.keyword.clone(), t.text.clone()))
        .chain(info.utf8_text.iter().filter_map(|t| Some((t.keyword.clone(), t.get_text().ok()?))))
        .collect();
    let (w, h) = (frame.width, frame.height);
    let px = &buf[..frame.buffer_size()];
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((image::RgbaImage::from_raw(w, h, rgba)?, text))
}

fn encode_png(img: &image::RgbaImage, text: &[(&str, String)]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width(), img.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        for (k, v) in text {
            enc.add_text_chunk(k.to_string(), v.clone()).ok()?;
        }
        let mut w = enc.write_header().ok()?;
        w.write_image_data(img.as_raw()).ok()?;
    }
    Some(out)
}

/// Write atomically (temporary file, then rename), with permissions 0600
/// in 0700 folders, as the spec asks. `size` is the file's apparent size.
pub fn write(root: &Path, bucket: Bucket, file: &Path, mtime: i64, size: u64, img: &image::RgbaImage) -> std::io::Result<()> {
    let text = [
        ("Thumb::URI", uri(file)),
        ("Thumb::MTime", mtime.to_string()),
        ("Thumb::Size", size.to_string()),
        ("Software", format!("Fractale {}", env!("CARGO_PKG_VERSION"))),
    ];
    let data = encode_png(img, &text).ok_or_else(|| std::io::Error::other("png encoding"))?;
    store(&path_in(root, bucket, file), &data)
}

/// Remember that this version of the file can't be thumbnailed.
pub fn write_failure(root: &Path, file: &Path, mtime: i64) -> std::io::Result<()> {
    let text = [("Thumb::URI", uri(file)), ("Thumb::MTime", mtime.to_string()), ("Software", format!("Fractale {}", env!("CARGO_PKG_VERSION")))];
    let data = encode_png(&image::RgbaImage::new(1, 1), &text).ok_or_else(|| std::io::Error::other("png encoding"))?;
    store(&fail_path_in(root, file), &data)
}

fn store(dest: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = dest.parent().unwrap();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!(".{}.fractale-{}.tmp", dest.file_name().unwrap().to_string_lossy(), std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
    }
    std::fs::rename(&tmp, dest)
}

/// Files inside the thumbnail cache itself are never thumbnailed.
pub fn is_inside(root: &Path, file: &Path) -> bool {
    file.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_and_name_follow_the_spec() {
        // The spec's own example.
        assert_eq!(uri(Path::new("/home/jens/photos/me.png")), "file:///home/jens/photos/me.png");
        assert_eq!(name("file:///home/jens/photos/me.png"), "c6ee772d9e49320e97ec29a7eb5b1697.png");
        assert_eq!(uri(Path::new("/a b/é#?.png")), "file:///a%20b/%C3%A9%23%3F.png");
    }

    #[test]
    fn round_trip_and_validation() {
        let root = std::env::temp_dir().join(format!("fractale-thumbcache-{}", std::process::id()));
        let file = Path::new("/some/where/pic.png");
        let mut img = image::RgbaImage::new(4, 3);
        img.put_pixel(1, 1, image::Rgba([10, 20, 30, 200]));
        write(&root, Bucket::Normal, file, 1234, 999, &img).unwrap();
        let back = read(&root, Bucket::Normal, file, 1234).unwrap();
        assert_eq!(back, img);
        assert!(read(&root, Bucket::Normal, file, 1235).is_none(), "stale mtime");
        assert!(read(&root, Bucket::Large, file, 1234).is_none());
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(path_in(&root, Bucket::Normal, file)).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
        write_failure(&root, file, 1234).unwrap();
        assert!(failed(&root, file, 1234));
        assert!(!failed(&root, file, 99));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
