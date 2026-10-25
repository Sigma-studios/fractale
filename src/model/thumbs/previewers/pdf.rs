//! PDFs: the first page, rendered in-process with hayro (pure Rust, no
//! poppler or pdfium to install). Encrypted files get the icon.

use super::super::{FileInfo, Previewer, Rendered};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, PixmapSettings, RenderCache, RenderSettings};

/// The whole file is read: larger PDFs get the icon.
const MAX_FILE: u64 = 256 << 20;

pub struct PdfPage;

impl Previewer for PdfPage {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "pdf"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let data = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let pdf = Pdf::new(data).map_err(|e| format!("{e:?}"))?;
        let pages = pdf.pages();
        let page = pages.first().ok_or("no pages")?;
        let (w, h) = page.render_dimensions();
        if !(w > 0.0 && h > 0.0) {
            return Err("empty page".into());
        }
        let k = max as f32 / w.max(h);
        let settings = PixmapSettings { x_scale: k, y_scale: k, bg_color: WHITE };
        let pixmap = render(page, &RenderCache::new(), &InterpreterSettings::default(), &RenderSettings::default(), &settings);
        // Opaque (white page): premultiplied is the same as straight.
        let (pw, ph) = (pixmap.width() as u32, pixmap.height() as u32);
        image::RgbaImage::from_raw(pw, ph, pixmap.data_as_u8_slice().to_vec()).map(Rendered::Still).ok_or_else(|| "bad size".into())
    }
}
