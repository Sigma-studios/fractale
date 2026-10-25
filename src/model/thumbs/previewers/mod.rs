//! The previewers, in the order they're tried.

pub mod audio;
pub mod gif;
pub mod images;
pub mod orchestre;
pub mod pixelle;
pub mod prisme;
#[cfg(target_os = "linux")]
pub mod thumbnailer;
pub mod xcf;

use super::Previewer;

/// Every previewer, most specific first.
pub fn registry() -> Vec<Box<dyn Previewer>> {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut v: Vec<Box<dyn Previewer>> = vec![
        Box::new(gif::Gif),
        Box::new(images::Images),
        Box::new(xcf::Xcf),
        Box::new(pixelle::Pixelle),
        Box::new(orchestre::Orchestre),
        Box::new(prisme::Prisme),
        Box::new(audio::Waveform),
    ];
    #[cfg(target_os = "linux")]
    v.extend(thumbnailer::installed());
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

/// Scale pixel art up with nearest-neighbour so it stays crisp, or down to
/// fit.
pub fn pixel_art(img: image::RgbaImage, max: u32) -> image::RgbaImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let k = (max / w.max(h)).max(1);
    if k > 1 {
        return image::imageops::resize(&img, w * k, h * k, image::imageops::FilterType::Nearest);
    }
    if w > max || h > max {
        let (fw, fh) = super::fit(w, h, max);
        return image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Nearest);
    }
    img
}
