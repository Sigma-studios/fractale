//! The previewers, in the order they're tried.

pub mod aseprite;
pub mod audio;
pub mod font;
pub mod gif;
pub mod html;
pub mod images;
pub mod office;
pub mod orchestre;
pub mod page;
pub mod pdf;
pub mod pixelle;
pub mod prisme;
pub mod svg;
#[cfg(target_os = "linux")]
pub mod thumbnailer;
pub mod video;
pub mod xcf;

use super::Previewer;

/// Every previewer, most specific first.
pub fn registry() -> Vec<Box<dyn Previewer>> {
    let mut v: Vec<Box<dyn Previewer>> = vec![
        Box::new(gif::Gif),
        Box::new(images::Images),
        Box::new(xcf::Xcf),
        Box::new(pixelle::Pixelle),
        Box::new(aseprite::Aseprite),
        Box::new(orchestre::Orchestre),
        Box::new(prisme::Prisme),
        Box::new(audio::Waveform),
        Box::new(svg::Svg),
        Box::new(pdf::PdfPage),
        Box::new(office::Office),
        Box::new(html::Html),
        Box::new(font::Font),
    ];
    // The desktop's thumbnailers first (on Linux), then ffmpeg for videos
    // nothing else took.
    #[cfg(target_os = "linux")]
    v.extend(thumbnailer::installed());
    if video::available() {
        v.push(Box::new(video::Video));
    }
    v
}

/// Shrink to fit `max` with a good filter (previews are small; aliasing
/// shows).
pub fn shrink(img: image::RgbaImage, max: u32) -> image::RgbaImage {
    let (w, h) = super::fit(img.width(), img.height(), max);
    if (w, h) == (img.width(), img.height()) {
        return img;
    }
    image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
}

/// Pixel art at its own size (the grid enlarges it by whole factors with
/// sharp pixels; a pre-enlarged image would blur when shown a little
/// smaller), or shrunk with nearest-neighbour when larger than `max`.
pub fn pixel_art(img: image::RgbaImage, max: u32) -> image::RgbaImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    if w > max || h > max {
        let (fw, fh) = super::fit(w, h, max);
        return image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Nearest);
    }
    img
}

/// A program on `PATH`.
pub fn find_program(name: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|c| c.is_file())
}

/// Run a program and return what it wrote, killing it (and what it
/// started) after `timeout`.
pub fn run_capture(cmd: &mut std::process::Command, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0).spawn().map_err(|e| e.to_string())?;
    let mut out = child.stdout.take().ok_or("no output")?;
    // Read on a thread: a full pipe would block the child forever.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > timeout {
            // SAFETY: plain syscall on the child's own (unreaped) group.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
            return Err("timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let buf = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    Ok(buf)
}
