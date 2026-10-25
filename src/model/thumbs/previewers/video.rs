//! Videos, when no desktop thumbnailer took them (macOS, or a Linux without
//! ffmpegthumbnailer): one frame from 10% in, extracted by `ffmpeg`.

use super::super::{FileInfo, Previewer, Rendered};
use std::process::Command;
use std::time::Duration;

pub const EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mpg", "mpeg", "ogv", "m2ts", "mts", "ts", "3gp"];

pub struct Video;

/// `ffmpeg` and `ffprobe` are on `PATH`.
pub fn available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|p| super::find_program(p).is_some())
}

/// A video's length in seconds and frame size, from ffprobe.
pub fn probe(path: &std::path::Path) -> Option<(f64, u32, u32)> {
    let out = super::run_capture(
        Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "default=noprint_wrappers=1"])
            .arg(path),
        Duration::from_secs(10),
    )
    .ok()?;
    let text = String::from_utf8_lossy(&out);
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).map(str::to_string);
    let duration = field("duration=").and_then(|d| d.parse().ok()).unwrap_or(0.0);
    Some((duration, field("width=")?.parse().ok()?, field("height=")?.parse().ok()?))
}

impl Previewer for Video {
    fn name(&self) -> &'static str {
        "video"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        EXTENSIONS.contains(&f.ext.as_str())
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        let (duration, _, _) = probe(&f.path).ok_or("not a video ffprobe can read")?;
        // Past the opening (often black), like other file managers.
        let at = if duration > 2.0 { duration * 0.1 } else { 0.0 };
        let png = super::run_capture(
            Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{at:.2}"), "-i"])
                .arg(&f.path)
                .args(["-frames:v", "1", "-vf", &format!("scale={max}:{max}:force_original_aspect_ratio=decrease"), "-f", "image2pipe", "-vcodec", "png", "pipe:1"]),
            Duration::from_secs(15),
        )?;
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.into_rgba8();
        Ok(Rendered::Still(img))
    }
}
