//! Still images, decoded in-process with the `image` crate, at the preview
//! size rather than in full where the format allows (JPEG decodes at 1/2,
//! 1/4 or 1/8 scale), with EXIF orientation applied.

use super::super::{FileInfo, Previewer, Rendered};
use image::ImageDecoder;
use std::io::BufReader;

/// Larger images get the type icon instead: decoding them would take too
/// long and too much memory for a tile.
const MAX_PIXELS: u64 = 100_000_000;

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "jpe", "webp", "bmp", "tif", "tiff", "qoi", "tga", "ico", "hdr", "pnm", "pbm", "pgm", "ppm"];

pub struct Images;

impl Previewer for Images {
    fn name(&self) -> &'static str {
        "images"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        EXTENSIONS.contains(&f.ext.as_str())
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size == 0 {
            return Err("empty file".into());
        }
        let reader = image::ImageReader::open(&f.path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
        let format = reader.format();
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let (w, h) = decoder.dimensions();
        if w as u64 * h as u64 > MAX_PIXELS {
            return Err(format!("{w}×{h} is too large to preview"));
        }
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let img = if format == Some(image::ImageFormat::Jpeg) {
            drop(decoder);
            jpeg_scaled(f, max)?
        } else {
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(1 << 30);
            decoder.set_limits(limits).map_err(|e| e.to_string())?;
            let dynimg = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
            dynimg.thumbnail(max, max).to_rgba8()
        };
        let mut img = image::DynamicImage::ImageRgba8(super::shrink(img, max));
        img.apply_orientation(orientation);
        Ok(Rendered::Still(img.to_rgba8()))
    }
}

/// JPEG through `jpeg-decoder`, which decodes straight at a reduced scale
/// (the DCT does the shrinking): a 24 MP photo for a 256 px tile is decoded
/// at 1/8 size.
fn jpeg_scaled(f: &FileInfo, max: u32) -> Result<image::RgbaImage, String> {
    let file = std::fs::File::open(&f.path).map_err(|e| e.to_string())?;
    let mut d = jpeg_decoder::Decoder::new(BufReader::new(file));
    d.read_info().map_err(|e| e.to_string())?;
    let max16 = max.min(u16::MAX as u32) as u16;
    d.scale(max16, max16).map_err(|e| e.to_string())?;
    let px = d.decode().map_err(|e| e.to_string())?;
    let info = d.info().ok_or("no image info")?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgba: Vec<u8> = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        jpeg_decoder::PixelFormat::L16 => px.chunks(2).flat_map(|c| [c[0], c[0], c[0], 255]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => px
            .chunks(4)
            .flat_map(|c| {
                // Adobe CMYK JPEGs are stored inverted.
                let k = c[3] as u32;
                let f = |x: u8| ((x as u32 * k) / 255) as u8;
                [f(c[0]), f(c[1]), f(c[2]), 255]
            })
            .collect(),
    };
    image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| "bad jpeg size".into())
}
