//! Animated GIFs (and still ones). The animation plays in the tile; only
//! its first frame goes to the shared cache, as other apps expect.

use super::super::{FileInfo, Previewer, Rendered};
use image::AnimationDecoder;
use std::time::Duration;

/// Keep a tile's animation small: frames, and total pixels.
const MAX_FRAMES: usize = 150;
const MAX_ANIMATION_BYTES: usize = 48 << 20;

pub struct Gif;

impl Previewer for Gif {
    fn name(&self) -> &'static str {
        "gif"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "gif"
    }

    fn animated(&self) -> bool {
        true
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        let file = std::io::BufReader::new(std::fs::File::open(&f.path).map_err(|e| e.to_string())?);
        let dec = image::codecs::gif::GifDecoder::new(file).map_err(|e| e.to_string())?;
        use image::ImageDecoder;
        let (w, h) = dec.dimensions();
        if w as u64 * h as u64 > 40_000_000 {
            return Err("too large".into());
        }
        let mut frames = Vec::new();
        let mut bytes = 0usize;
        for frame in dec.into_frames() {
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            // Browsers treat very short delays as 100 ms.
            let ms = num.checked_div(den).map_or(100, |ms| ms.max(20));
            let img = super::shrink(frame.into_buffer(), max);
            bytes += img.as_raw().len();
            frames.push((img, Duration::from_millis(ms as u64)));
            if frames.len() >= MAX_FRAMES || bytes >= MAX_ANIMATION_BYTES {
                break;
            }
        }
        match frames.len() {
            0 => Err("no frames".into()),
            1 => Ok(Rendered::Still(frames.pop().unwrap().0)),
            _ => Ok(Rendered::Animated(frames)),
        }
    }
}
