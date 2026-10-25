//! Fonts (TrueType, OpenType, collections): a specimen drawn with the font
//! itself: "Aa", a sample line and the family's name.

use super::super::{FileInfo, Previewer, Rendered};
use resvg::{tiny_skia, usvg};
use std::sync::Arc;

const MAX_FILE: u64 = 64 << 20;

pub struct Font;

impl Previewer for Font {
    fn name(&self) -> &'static str {
        "font"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        matches!(f.ext.as_str(), "ttf" | "otf" | "ttc" | "otc")
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let data = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(data);
        let face = db.faces().next().ok_or("not a font")?;
        let family = face.families.first().map(|(n, _)| n.clone()).ok_or("no family name")?;
        let (weight, style) = (face.weight.0, if face.style == usvg::fontdb::Style::Normal { "normal" } else { "italic" });
        let esc = super::page::escape(&family);
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><rect width="200" height="200" rx="10" fill="#ffffff"/><g font-family="{esc}" font-weight="{weight}" font-style="{style}" fill="#1a1a19"><text x="100" y="98" font-size="86" text-anchor="middle">Aa</text><text x="100" y="138" font-size="17" text-anchor="middle">Quick fox 123</text></g><text x="100" y="178" font-size="11" text-anchor="middle" fill="#8b8a83" font-family="{fallback}">{esc}</text></svg>"##,
            fallback = "DejaVu Sans, Helvetica, Arial, sans-serif"
        );
        // This font, plus the system's for the name line.
        let mut all = (*super::svg::options().fontdb).clone();
        for face in db.faces() {
            if let Some((src, _)) = db.face_source(face.id) {
                all.load_font_source(src);
            }
        }
        let options = usvg::Options { fontdb: Arc::new(all), ..Default::default() };
        let tree = usvg::Tree::from_str(&svg, &options).map_err(|e| e.to_string())?;
        let k = max as f32 / 200.0;
        let n = ((200.0 * k).round() as u32).max(1);
        let mut pixmap = tiny_skia::Pixmap::new(n, n).ok_or("empty")?;
        resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pixmap.as_mut());
        let rgba: Vec<u8> = pixmap.pixels().iter().flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        }).collect();
        image::RgbaImage::from_raw(n, n, rgba).map(Rendered::Still).ok_or_else(|| "bad size".into())
    }
}
