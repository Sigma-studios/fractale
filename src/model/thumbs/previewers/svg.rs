//! SVG (and gzipped SVGZ) drawings, rendered with resvg at the preview size.

use super::super::{FileInfo, Previewer, Rendered};
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

const MAX_FILE: u64 = 32 << 20;

pub struct Svg;

/// System fonts for text in drawings, loaded once (it takes a while).
pub fn options() -> &'static usvg::Options<'static> {
    static OPTIONS: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        usvg::Options { fontdb: Arc::new(db), ..Default::default() }
    })
}

impl Previewer for Svg {
    fn name(&self) -> &'static str {
        "svg"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "svg" || f.ext == "svgz"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let data = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let tree = usvg::Tree::from_data(&data, options()).map_err(|e| e.to_string())?;
        let size = tree.size();
        let k = max as f32 / size.width().max(size.height()).max(1.0);
        let (w, h) = (((size.width() * k).round() as u32).max(1), ((size.height() * k).round() as u32).max(1));
        let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("empty drawing")?;
        resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pixmap.as_mut());
        let rgba: Vec<u8> = pixmap.pixels().iter().flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        }).collect();
        image::RgbaImage::from_raw(w, h, rgba).map(Rendered::Still).ok_or_else(|| "bad size".into())
    }
}
