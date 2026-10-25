//! Aseprite sprites (`.aseprite`, `.ase`), composited by the asefile crate:
//! frame 0, or every frame as an animation when there are several. Pixel
//! art: scaled by whole factors with nearest-neighbour (see `pixel_art`).

use super::super::{FileInfo, Previewer, Rendered};
use std::time::Duration;

const MAX_FILE: u64 = 64 << 20;
/// asefile allocates the canvas for every frame: check the header first.
const MAX_PIXELS: u64 = 16 << 20;
const MAX_FRAMES: u32 = 150;
/// All frames together, at preview size.
const MAX_BYTES: usize = 48 << 20;

pub struct Aseprite;

impl Previewer for Aseprite {
    fn name(&self) -> &'static str {
        "aseprite"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "aseprite" || f.ext == "ase"
    }

    fn animated(&self) -> bool {
        true
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let bytes = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        // Header: size u32, magic 0xA5E0, frames u16, width u16, height u16.
        if bytes.len() < 128 || bytes[4..6] != [0xE0, 0xA5] {
            return Err("not an Aseprite file".into());
        }
        let (w, h) = (u16::from_le_bytes([bytes[8], bytes[9]]) as u64, u16::from_le_bytes([bytes[10], bytes[11]]) as u64);
        if w == 0 || h == 0 || w * h > MAX_PIXELS {
            return Err(format!("{w}×{h} is too large to preview"));
        }
        let ase = asefile::AsepriteFile::read(&bytes[..]).map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        let mut used = 0usize;
        for i in 0..ase.num_frames().min(MAX_FRAMES) {
            let frame = ase.frame(i);
            let img = frame.image();
            let (fw, fh) = img.dimensions();
            let img = image::RgbaImage::from_raw(fw, fh, img.into_raw()).ok_or("bad frame")?;
            let img = super::pixel_art(img, max);
            used += img.len();
            frames.push((img, Duration::from_millis(frame.duration().max(10) as u64)));
            if used > MAX_BYTES {
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
