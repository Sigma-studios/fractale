//! Prisme documents (`.prisme`): the preview image the file carries.
//!
//! The container is `PRISME\0\x01`, a u32 version, a u32 chunk count, then a
//! table of contents of {tag, offset u64, length u64}, all little-endian.
//! The `PREV` chunk is width u32, height u32, then LZ4-compressed RGBA
//! (`lz4_flex::compress`). Rendering the layers instead would mean
//! reimplementing Prisme's compositing, so files without `PREV` get the
//! type icon. (Prisme can write it, `container::save_with_preview`, but the
//! app saves without one as of 26ea8cb.)

use super::super::{FileInfo, Previewer, Rendered};

const MAGIC: &[u8; 8] = b"PRISME\0\x01";
const HEADER_LEN: usize = 16;
const TOC_ENTRY_LEN: usize = 20;

pub struct Prisme;

impl Previewer for Prisme {
    fn name(&self) -> &'static str {
        "prisme"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "prisme"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        let bytes = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let (w, h, rgba) = read_preview(&bytes).ok_or("no preview in this file")?;
        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("bad preview")?;
        Ok(Rendered::Still(super::shrink(img, max)))
    }
}

pub fn read_preview(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
        return None;
    }
    let u32_at = |b: &[u8], i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let u64_at = |b: &[u8], i: usize| Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?));
    let count = u32_at(bytes, 12)? as usize;
    let toc_end = count.checked_mul(TOC_ENTRY_LEN)?.checked_add(HEADER_LEN).filter(|&e| e <= bytes.len())?;
    let chunk = bytes[HEADER_LEN..toc_end].as_chunks::<TOC_ENTRY_LEN>().0.iter().find(|e| &e[..4] == b"PREV")?;
    let (off, len) = (u64_at(chunk, 4)?, u64_at(chunk, 12)?);
    let end = off.checked_add(len).filter(|&end| end <= bytes.len() as u64 && off >= toc_end as u64)?;
    let p = &bytes[off as usize..end as usize];
    let (w, h) = (u32_at(p, 0)?, u32_at(p, 4)?);
    let n = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    // Prisme's own limit: anything larger is corrupt, don't allocate for it.
    if n == 0 || n > 64 << 20 {
        return None;
    }
    let rgba = lz4_flex::decompress(&p[8..], n).ok()?;
    (rgba.len() == n).then_some((w, h, rgba))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A container with a `DOC ` chunk and a `PREV` chunk, like Prisme's.
    pub fn file_with_preview(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut prev = Vec::new();
        prev.extend(w.to_le_bytes());
        prev.extend(h.to_le_bytes());
        prev.extend(lz4_flex::compress(rgba));
        let doc = b"not looked at".to_vec();
        let mut out = MAGIC.to_vec();
        out.extend(1u32.to_le_bytes());
        out.extend(2u32.to_le_bytes());
        let mut off = (HEADER_LEN + 2 * TOC_ENTRY_LEN) as u64;
        for (tag, data) in [(b"DOC ", &doc), (b"PREV", &prev)] {
            out.extend(tag);
            out.extend(off.to_le_bytes());
            out.extend((data.len() as u64).to_le_bytes());
            off += data.len() as u64;
        }
        out.extend(&doc);
        out.extend(&prev);
        out
    }

    #[test]
    fn reads_the_preview_chunk() {
        let rgba: Vec<u8> = (0..4 * 6 * 4).map(|i| i as u8).collect();
        let f = file_with_preview(4, 6, &rgba);
        assert_eq!(read_preview(&f), Some((4, 6, rgba)));
    }

    #[test]
    fn rejects_damage() {
        let rgba = vec![7u8; 4 * 6 * 4];
        let f = file_with_preview(4, 6, &rgba);
        for cut in [0, 8, 15, 30, f.len() - 1] {
            assert_eq!(read_preview(&f[..cut]), None, "cut at {cut}");
        }
        let mut huge = f.clone();
        let p = f.len() - (8 + lz4_flex::compress(&rgba).len());
        huge[p..p + 4].copy_from_slice(&100_000u32.to_le_bytes());
        huge[p + 4..p + 8].copy_from_slice(&100_000u32.to_le_bytes());
        assert_eq!(read_preview(&huge), None);
        let mut count = f.clone();
        count[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_preview(&count), None);
    }
}
