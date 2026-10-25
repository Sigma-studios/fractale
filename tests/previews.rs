//! Previews in the grid: real ones for what can be previewed, the type icon
//! for the rest, quickly and without blocking frames, and the shared cache
//! written the way the freedesktop spec says.

mod common;

use common::*;
use fractale::model::thumbs::cache::{self, Bucket};
use fractale::ui::previews::Status;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

/// A PNG that claims to be 50000×50000 and stops after its header.
fn huge_png(path: &Path) {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(50_000u32.to_be_bytes());
    ihdr.extend(50_000u32.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    let mut crc = flate2::Crc::new();
    crc.update(&ihdr);
    let mut f = b"\x89PNG\r\n\x1a\n".to_vec();
    f.extend(13u32.to_be_bytes());
    f.extend(&ihdr);
    f.extend(crc.sum().to_be_bytes());
    std::fs::write(path, f).unwrap();
}

fn gif(path: &Path) {
    use image::codecs::gif::{GifEncoder, Repeat};
    let mut enc = GifEncoder::new(std::fs::File::create(path).unwrap());
    enc.set_repeat(Repeat::Infinite).unwrap();
    for c in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]] {
        let img = image::RgbaImage::from_pixel(40, 30, image::Rgba(c));
        enc.encode_frame(image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1))).unwrap();
    }
}

#[test]
fn previews_icons_and_the_shared_cache() {
    let s = Scratch::new("previews");
    image::RgbaImage::from_pixel(300, 200, image::Rgba([200, 30, 30, 255])).save(s.0.join("red.png")).unwrap();
    image::RgbImage::from_fn(2000, 1500, |x, y| image::Rgb([(x / 8) as u8, (y / 6) as u8, 90])).save(s.0.join("photo.jpg")).unwrap();
    gif(&s.0.join("anim.gif"));
    std::fs::write(s.0.join("broken.jpg"), b"this is not a jpeg at all").unwrap();
    std::fs::write(s.0.join("empty.png"), b"").unwrap();
    huge_png(&s.0.join("huge.png"));
    s.file("notes.txt", 100);

    let mut h = window(&s.0);
    listed(&mut h);
    let status = |h: &H, n: &str| h.state().previews.status(&s.0.join(n));
    let took = until(&mut h, Duration::from_secs(20), "previews", |w| {
        ["red.png", "photo.jpg", "anim.gif", "broken.jpg", "huge.png"].iter().all(|n| matches!(w.previews.status(&s.0.join(n)), Status::Ready { .. } | Status::Failed))
    });
    eprintln!("previews and failures in {took:?}");
    let ready = |h: &H, n: &str| match status(h, n) {
        Status::Ready { frames, size } => (frames, size),
        other => panic!("{n}: {other:?}"),
    };
    // Normal bucket (96 px tiles at 1×): fitted into 128, never grown.
    assert_eq!(ready(&h, "red.png"), (1, egui::vec2(128.0, 85.0)));
    assert_eq!(ready(&h, "photo.jpg").1, egui::vec2(128.0, 96.0));
    assert_eq!(ready(&h, "anim.gif").0, 3, "the GIF animates");
    assert_eq!(status(&h, "broken.jpg"), Status::Failed);
    assert_eq!(status(&h, "huge.png"), Status::Failed);
    assert_eq!(status(&h, "empty.png"), Status::Unknown, "empty files aren't asked for");
    assert_eq!(status(&h, "notes.txt"), Status::Failed, "no previewer: the icon");

    // The shared cache: valid entries, private permissions, failures
    // recorded where the spec says.
    let root = thumbnails();
    for n in ["red.png", "photo.jpg", "anim.gif"] {
        let p = s.0.join(n);
        let mtime = std::fs::metadata(&p).unwrap().mtime();
        let img = cache::read(&root, Bucket::Normal, &p, mtime).unwrap_or_else(|| panic!("{n} cached"));
        assert!(img.width().max(img.height()) <= 128);
        assert!(cache::read(&root, Bucket::Normal, &p, mtime + 1).is_none(), "a changed file isn't served the old preview");
        let mode = std::fs::metadata(cache::path_in(&root, Bucket::Normal, &p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{n}");
    }
    let broken = s.0.join("broken.jpg");
    assert!(cache::failed(&root, &broken, std::fs::metadata(&broken).unwrap().mtime()));
    assert!(!cache::path_in(&root, Bucket::Normal, &s.0.join("notes.txt")).exists());

    // A second window gets them from the cache.
    let mut h2 = window(&s.0);
    listed(&mut h2);
    let again = until(&mut h2, Duration::from_secs(10), "cached previews", |w| matches!(w.previews.status(&s.0.join("photo.jpg")), Status::Ready { .. }));
    eprintln!("from the cache in {again:?}");
}

/// The visible files' indexes in the grid, from last frame's scroll state.
fn visible(w: &fractale::ui::Window) -> std::ops::Range<usize> {
    let s = w.scroll[1];
    let first = (s.offset / s.row_height).floor() as usize * s.columns;
    let last = ((s.offset + s.viewport) / s.row_height).ceil() as usize * s.columns;
    first..last.min(w.explorer.files.len())
}

fn all_shown(w: &fractale::ui::Window) -> bool {
    visible(w).all(|i| {
        let e = w.explorer.entry(w.explorer.files[i]);
        matches!(w.previews.status(&w.explorer.folder.join(&e.name)), Status::Ready { .. } | Status::Failed)
    })
}

/// 5,000 photos: the first screen of previews within a second, frames at
/// 144 Hz while scrolling. Needs `FRACTALE_IMAGES=target/fixtures/images5k`
/// (`scripts/fixtures.sh` makes it) and a release build:
/// `FRACTALE_IMAGES=… cargo test --release --test previews -- --nocapture five_thousand`.
#[test]
fn five_thousand_images() {
    let Some(dir) = std::env::var_os("FRACTALE_IMAGES") else {
        eprintln!("FRACTALE_IMAGES not set: skipped");
        return;
    };
    let dir = Path::new(&dir).canonicalize().unwrap();
    // A cold cache, so previews are really decoded.
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("bench-thumbnails");
    let _ = std::fs::remove_dir_all(&root);
    let mut w = fractale::ui::Window::new(fractale::ui::Start {
        folder: dir.clone(),
        select: None,
        sources: fractale::model::explorer::Sources { socket: "/nonexistent/x.sock".into(), retry: Duration::from_secs(3600), thumbnails: root.clone() },
    });
    w.launch = false;
    let mut h = builder().with_size(egui::vec2(2560.0, 1440.0)).build_ui_state(|ui, w: &mut fractale::ui::Window| w.ui(ui), w);
    let start = Instant::now();
    until(&mut h, Duration::from_secs(10), "listing", |w| w.explorer.is_current() && !w.explorer.files.is_empty());
    let listed = start.elapsed();
    h.step();
    let shown = visible(h.state()).len();
    until(&mut h, Duration::from_secs(30), "first screen", all_shown);
    let first = start.elapsed();
    eprintln!("5k images: listed in {listed:?}, first screen ({shown} previews) in {first:?}");

    // Scroll at 144 Hz for 4 s: 300 px a frame (fast flicking), or
    // `FRACTALE_SCROLL_PX`.
    let step: f32 = std::env::var("FRACTALE_SCROLL_PX").ok().and_then(|v| v.parse().ok()).unwrap_or(300.0);
    // The wheel scrolls what's under the pointer.
    h.event(egui::Event::PointerMoved(egui::pos2(1800.0, 700.0)));
    h.step();
    h.state_mut().stats.worst = std::time::Duration::ZERO;
    let frame = Duration::from_micros(6944);
    let mut missing_frames = 0;
    let scroll_start = Instant::now();
    let mut frames = 0;
    let top = h.state().scroll[1].offset;
    while scroll_start.elapsed() < Duration::from_secs(4) {
        let t = Instant::now();
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -step), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel });
        h.step();
        frames += 1;
        if !all_shown(h.state()) {
            missing_frames += 1;
        }
        if let Some(rest) = frame.checked_sub(t.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    let worst = h.state().stats.worst;
    let travelled = h.state().scroll[1].offset - top;
    assert!(travelled > step * 100.0, "scrolled {travelled} px");
    let settle = Instant::now();
    if std::env::var_os("FRACTALE_TRACE").is_some() {
        while !all_shown(h.state()) && settle.elapsed() < Duration::from_secs(3) {
            h.step();
            let w = h.state();
            let missing = visible(w).filter(|&i| !matches!(w.previews.status(&w.explorer.folder.join(&w.explorer.entry(w.explorer.files[i]).name)), Status::Ready { .. } | Status::Failed)).count();
            eprintln!("{:?}: {missing} missing, {} pending", settle.elapsed(), w.previews.pending());
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    until(&mut h, Duration::from_secs(30), "screen after scrolling", all_shown);
    let settled = settle.elapsed();
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct we pass.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let maxrss_mb = if cfg!(target_os = "macos") { ru.ru_maxrss as f64 / 1e6 } else { ru.ru_maxrss as f64 / 1e3 };
    let p = &h.state().previews;
    eprintln!(
        "scrolling: {frames} frames over {travelled:.0} px, worst {worst:?}, {missing_frames} frames with some tile still on its icon; screen complete {settled:?} after stopping; {} textures, {:.0} MB of texture, peak RSS {maxrss_mb:.0} MB",
        p.textures(),
        p.memory() as f64 / 1e6
    );
    if !cfg!(debug_assertions) {
        assert!(first < Duration::from_secs(1), "first screen in {first:?}");
        assert!(worst < frame, "worst frame {worst:?}");
    }
}

/// Videos go through the desktop's thumbnailers (ffmpegthumbnailer here).
/// Needs `ffmpeg` to make the clip and an installed video thumbnailer;
/// skipped otherwise.
#[test]
fn video_through_the_system_thumbnailer() {
    let all = fractale::model::thumbs::previewers::registry();
    let s = Scratch::new("video");
    let clip = s.0.join("clip.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=10", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .is_ok_and(|s| s.success());
    let info = fractale::model::thumbs::FileInfo::new(clip.clone(), std::fs::metadata(&clip).map_or(0, |m| m.len()), 0);
    let Some(p) = all.iter().find(|p| made && p.accepts(&info)) else {
        eprintln!("no ffmpeg or no video thumbnailer: skipped");
        return;
    };
    let start = Instant::now();
    let r = p.render(&info, 256).unwrap();
    let img = r.still();
    eprintln!("{} made a {}×{} preview in {:?}", p.name(), img.width(), img.height(), start.elapsed());
    assert!(img.width().max(img.height()) <= 256 && img.width() >= 64);
}
