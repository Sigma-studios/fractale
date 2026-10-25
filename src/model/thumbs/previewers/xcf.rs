//! Flattened previews of GIMP `.xcf` files.
//!
//! The `xcf` crate (0.4) reads RGB only, without compression, and has
//! `unimplemented!` paths, so this is a small reader of our own. It decodes the first (full-size) level of each visible
//! layer and composites the layers bottom to top with "normal" blending.
//!
//! Supported: "gimp xcf file" and v001 onwards (64-bit offsets from v011),
//! RGB, Grayscale and Indexed images, every precision GIMP writes (8/16/32-bit
//! integer, half, float and double, linear or gamma), uncompressed, RLE and
//! zlib tiles, and layer visibility, opacity, offsets and masks.
//!
//! Approximated or ignored: all blend modes are drawn as normal, colour
//! profiles are ignored (everything is treated as sRGB), and channels, paths
//! and v020+ non-destructive filters are skipped. Text, vector and link
//! layers show the pixels GIMP stored for them.
//!
//! Layer groups: GIMP saves each group's merged projection as the group's own
//! pixels. We draw a group like a plain layer and skip its children (they're
//! the layers that carry PROP_ITEM_PATH). That gets group opacity, visibility
//! and masks right without rebuilding the layer tree.
//!
//! Every count, size and offset comes from an untrusted file. Reads are
//! bounds-checked, the canvas is capped at 100 MP, and the total decoding is
//! capped by a budget proportional to the file size. Anything unexpected
//! returns `Err`; nothing panics.

use super::super::{FileInfo, Previewer, Rendered};
use flate2::{Decompress, FlushDecompress};
use image::imageops::{self, FilterType::Triangle};
use image::{Rgba, RgbaImage};
use std::sync::OnceLock;

const MAX_CANVAS: u64 = 100_000_000;
const MAX_SIDE: u32 = 524_288; // GIMP_MAX_IMAGE_SIZE
const MAX_LAYERS: usize = 10_000;
const TILE: u32 = 64;
/// Work allowed per input byte, counted in decoded tile pixels (properties
/// and tile tables count too). Tiles GIMP writes take at least ~12 bytes per
/// 4096 pixels, so real files stay well under this. A crafted file can point
/// thousands of layers at the same few compressed tiles, and this keeps it
/// from decoding them over and over.
const WORK_PER_BYTE: u64 = 512;

type Res<T> = Result<T, String>;

/// The whole file is read: XCFs over this get the icon.
const MAX_FILE: u64 = 1 << 30;

pub struct Xcf;

impl Previewer for Xcf {
    fn name(&self) -> &'static str {
        "xcf"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "xcf"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let bytes = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        render(&bytes, max).map(Rendered::Still)
    }
}

/// The composited canvas, shrunk so its larger side is at most `max` (never enlarged).
pub fn render(bytes: &[u8], max: u32) -> Result<RgbaImage, String> {
    if max == 0 {
        return Err("max must be positive".into());
    }
    let mut r = Rd::new(bytes, false);
    let magic = r.take(14).map_err(|_| "not an XCF file")?;
    if &magic[..9] != b"gimp xcf " || magic[13] != 0 {
        return Err("not an XCF file".into());
    }
    let version = match &magic[9..13] {
        b"file" => 0,
        [b'v', d @ ..] if d.iter().all(u8::is_ascii_digit) => d.iter().fold(0, |v, c| v * 10 + u32::from(c - b'0')),
        _ => return Err("unknown XCF version".into()),
    };
    r.wide = version >= 11;
    let (w, h, base) = (r.u32()?, r.u32()?, r.u32()?);
    if base > 2 {
        return Err("unknown image type".into());
    }
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_CANVAS {
        return Err(format!("unsupported canvas size {w}x{h}"));
    }
    let prec = if version >= 4 {
        precision(version, r.u32()?).ok_or("unknown precision")?
    } else {
        GAMMA_8
    };

    let mut work = (bytes.len() as u64 * WORK_PER_BYTE).clamp(1 << 24, 1 << 33);
    let (mut comp, mut cmap) = (0u8, Vec::new());
    loop {
        spend(&mut work, 64)?;
        let (ty, len) = (r.u32()?, r.u32()? as usize);
        match ty {
            0 => break,
            // GIMP ignores the payload length here: version 0 wrote a wrong one.
            1 => {
                let n = r.u32()? as usize;
                if n > 256 {
                    return Err("colormap too large".into());
                }
                cmap = if version == 0 {
                    // v0 colormaps were saved broken; GIMP substitutes a gray ramp.
                    r.take(n)?;
                    (0..n).map(|i| [i as u8; 3]).collect()
                } else {
                    r.take(3 * n)?.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
                };
            }
            17 => comp = *r.take(len)?.first().ok_or("bad compression property")?,
            _ => {
                r.take(len)?;
            }
        }
    }
    let mut layers = Vec::new();
    loop {
        match r.ptr()? {
            0 => break,
            _ if layers.len() == MAX_LAYERS => return Err("too many layers".into()),
            p => layers.push(p),
        }
    }

    // Large images are composited on a point-sampled grid at least twice the
    // target size, so a 100 MP file never needs a 100 MP buffer for a thumbnail.
    let step = (w.max(h) / max.saturating_mul(2)).max(1);
    let mut canvas = RgbaImage::new(w.div_ceil(step), h.div_ceil(step));
    let ctx = Ctx {
        rd: Rd::new(bytes, r.wide),
        le: version < 12,
        prec,
        comp,
        cmap,
        step,
    };
    // The list runs top to bottom.
    for &at in layers.iter().rev() {
        draw_layer(&ctx, at, &mut canvas, &mut work)?;
    }

    let larger = u64::from(w.max(h));
    if larger <= u64::from(max) {
        return Ok(canvas);
    }
    let fit = |side: u32| ((u64::from(side) * u64::from(max) + larger / 2) / larger).max(1) as u32;
    Ok(imageops::resize(&canvas, fit(w), fit(h), Triangle))
}

#[derive(Clone, Copy)]
enum Depth {
    U8,
    U16,
    U32,
    Half,
    Float,
    Double,
}

#[derive(Clone, Copy)]
struct Precision {
    depth: Depth,
    linear: bool,
}

const GAMMA_8: Precision = Precision {
    depth: Depth::U8,
    linear: false,
};

impl Precision {
    fn bytes(self) -> usize {
        match self.depth {
            Depth::U8 => 1,
            Depth::U16 | Depth::Half => 2,
            Depth::U32 | Depth::Float => 4,
            Depth::Double => 8,
        }
    }
}

fn precision(version: u32, p: u32) -> Option<Precision> {
    use Depth::*;
    // v4..v6 were development formats with their own numbering; v7+ store
    // GimpPrecision, where 75 is GIMP 3's "perceptual" (sRGB-like) TRC.
    let (depth, linear) = match (version, p) {
        (4, 0) => (U8, false),
        (4, 1) => (U16, false),
        (4, 2) => (U32, true),
        (4, 3) => (Half, true),
        (4, 4) => (Float, true),
        (4, _) => return None,
        _ => {
            let old = version <= 6;
            let d = match (p / 100, old) {
                (1, _) => U8,
                (2, _) => U16,
                (3, _) => U32,
                (4, true) | (5, false) => Half,
                (5, true) | (6, false) => Float,
                (7, false) => Double,
                _ => return None,
            };
            match (p % 100, old) {
                (0, _) => (d, true),
                (50, _) | (75, false) => (d, false),
                _ => return None,
            }
        }
    };
    Some(Precision { depth, linear })
}

/// A bounds-checked big-endian cursor; `wide` selects 64-bit offsets.
#[derive(Clone, Copy)]
struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
    wide: bool,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8], wide: bool) -> Self {
        Rd { b, pos: 0, wide }
    }
    fn seek(&self, pos: u64) -> Res<Rd<'a>> {
        let pos = usize::try_from(pos)
            .ok()
            .filter(|&p| p < self.b.len())
            .ok_or("offset out of range")?;
        Ok(Rd { pos, ..*self })
    }
    fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or("truncated file")?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Res<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn ptr(&mut self) -> Res<u64> {
        if !self.wide {
            return self.u32().map(u64::from);
        }
        Ok(u64::from(self.u32()?) << 32 | u64::from(self.u32()?))
    }
    fn skip_string(&mut self) -> Res<()> {
        let n = self.u32()? as usize;
        self.take(n).map(drop)
    }
}

fn spend(work: &mut u64, n: u64) -> Res<()> {
    *work = work.checked_sub(n).ok_or("file needs too much work to preview")?;
    Ok(())
}

/// Calls `f` with each property's type and a reader over its payload.
fn props<'a>(r: &mut Rd<'a>, work: &mut u64, mut f: impl FnMut(u32, &mut Rd<'a>) -> Res<()>) -> Res<()> {
    loop {
        spend(work, 64)?;
        let (ty, len) = (r.u32()?, r.u32()? as usize);
        if ty == 0 {
            return Ok(());
        }
        f(ty, &mut Rd::new(r.take(len)?, r.wide))?;
    }
}

struct Ctx<'a> {
    rd: Rd<'a>,
    /// Before v012, high-bit-depth components were written in host order,
    /// which for every GIMP build that shipped those versions was little-endian.
    le: bool,
    prec: Precision,
    comp: u8,
    cmap: Vec<[u8; 3]>,
    step: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Rgb,
    Gray,
    Indexed,
}

fn draw_layer(c: &Ctx, at: u64, canvas: &mut RgbaImage, work: &mut u64) -> Res<()> {
    let mut r = c.rd.seek(at)?;
    let (w, h, kind) = (r.u32()?, r.u32()?, r.u32()?);
    r.skip_string()?;
    let (mut visible, mut opacity, mut mode, mut space) = (true, 1.0f32, 0, 0i32);
    let (mut x, mut y, mut apply_mask, mut nested) = (0i32, 0i32, true, false);
    props(&mut r, work, |ty, p| {
        match ty {
            // PROP_FLOAT_OPACITY follows PROP_OPACITY, so the last one wins as in GIMP.
            6 => opacity = p.u32()?.min(255) as f32 / 255.0,
            33 => opacity = f32::from_bits(p.u32()?),
            7 => mode = p.u32()?,
            8 => visible = p.u32()? != 0,
            11 => apply_mask = p.u32()? != 0,
            15 => (x, y) = (p.u32()? as i32, p.u32()? as i32),
            30 => nested = true,
            36 => space = p.u32()? as i32,
            _ => {}
        }
        Ok(())
    })?;
    if !visible || nested || opacity.is_nan() || opacity <= 0.0 || w == 0 || h == 0 {
        return Ok(());
    }
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err("layer too large".into());
    }
    let (family, channels) = match kind {
        0 | 1 => (Family::Rgb, 3 + kind as usize),
        2 | 3 => (Family::Gray, kind as usize - 1),
        4 | 5 => (Family::Indexed, kind as usize - 3),
        _ => return Err("unknown layer type".into()),
    };
    let prec = if family == Family::Indexed { GAMMA_8 } else { c.prec };
    let (bpc, bpp) = (prec.bytes(), prec.bytes() * channels);
    let (hier, mask_at) = (r.ptr()?, r.ptr()?);
    let Some(tiles) = level(c, hier, bpp, w, h, work)? else {
        return Ok(()); // no tiles: fully transparent
    };
    let mask = if apply_mask && mask_at != 0 {
        let mut m = c.rd.seek(mask_at)?;
        if (m.u32()?, m.u32()?) != (w, h) {
            return Err("mask size differs from its layer".into());
        }
        m.skip_string()?;
        props(&mut m, work, |_, _| Ok(()))?;
        match level(c, m.ptr()?, bpc, w, h, work)? {
            None => return Ok(()), // an empty mask hides everything
            some => some,
        }
    } else {
        None
    };
    // Every blend mode is drawn as normal; a preview can live without
    // multiply or screen. Only the mode's compositing space is kept:
    // GIMP 2.10's own modes composite in linear light, the legacy ones (< 23)
    // in gamma space. PROP_COMPOSITE_SPACE stores the effective space, negated
    // when it was "auto".
    let linear = match space.unsigned_abs() {
        1 | 3 => true,
        2 | 4 => false,
        _ => mode >= 23,
    };

    let opacity = opacity.min(1.0);
    let step = i64::from(c.step);
    let (cw, ch) = canvas.dimensions();
    // Canvas cells whose sample point lies in [from, from + len).
    let cells = |from: i64, len: u32, limit: u32| {
        let lo = (from.max(0) as u64).div_ceil(step as u64);
        let hi = ((from + i64::from(len)).max(0) as u64)
            .div_ceil(step as u64)
            .min(u64::from(limit));
        lo as u32..hi.max(lo) as u32
    };
    let (mut buf, mut mbuf) = (
        vec![0u8; (TILE * TILE) as usize * bpp],
        vec![0u8; (TILE * TILE) as usize * bpc],
    );
    let cols = w.div_ceil(TILE);
    for ty in 0..h.div_ceil(TILE) {
        for tx in 0..cols {
            let (tw, th) = (TILE.min(w - tx * TILE), TILE.min(h - ty * TILE));
            let (x0, y0) = (i64::from(x) + i64::from(tx * TILE), i64::from(y) + i64::from(ty * TILE));
            let (xs, ys) = (cells(x0, tw, cw), cells(y0, th, ch));
            if xs.is_empty() || ys.is_empty() {
                continue;
            }
            spend(work, u64::from(tw * th))?;
            let index = u64::from(ty * cols + tx);
            let px = &mut buf[..(tw * th) as usize * bpp];
            decode(c, tile_ptr(c, &tiles, index)?, px, bpp)?;
            let mpx = &mut mbuf[..(tw * th) as usize * bpc];
            if let Some(m) = &mask {
                decode(c, tile_ptr(c, m, index)?, mpx, bpc)?;
            }
            for cy in ys.clone() {
                let row = (i64::from(cy) * step - y0) as usize * tw as usize;
                for cx in xs.clone() {
                    let k = row + (i64::from(cx) * step - x0) as usize;
                    let (rgb, mut a) = pixel(c, family, prec, channels, &px[k * bpp..(k + 1) * bpp]);
                    a *= opacity;
                    if mask.is_some() {
                        a *= sample(&mpx[k * bpc..], prec, c.le);
                    }
                    over(canvas.get_pixel_mut(cx, cy), rgb, a, linear);
                }
            }
        }
    }
    Ok(())
}

/// Follows a hierarchy to its first level and returns a reader on the
/// level's tile offset table, or None when the level holds no tiles.
fn level<'a>(c: &Ctx<'a>, at: u64, bpp: usize, w: u32, h: u32, work: &mut u64) -> Res<Option<Rd<'a>>> {
    let mut r = c.rd.seek(at)?;
    if (r.u32()?, r.u32()?, r.u32()? as usize) != (w, h, bpp) {
        return Err("pixel data does not match its layer".into());
    }
    let mut l = c.rd.seek(r.ptr()?)?;
    if (l.u32()?, l.u32()?) != (w, h) {
        return Err("level size does not match its layer".into());
    }
    let ntiles = u64::from(w.div_ceil(TILE)) * u64::from(h.div_ceil(TILE));
    spend(work, ntiles)?;
    // The whole offset table must be in the file; this also bounds ntiles.
    let table = ntiles * if l.wide { 8 } else { 4 };
    if table > (l.b.len() - l.pos) as u64 {
        return Err("truncated tile table".into());
    }
    let mut first = l;
    Ok((first.ptr()? != 0).then_some(l))
}

fn tile_ptr(c: &Ctx, table: &Rd, index: u64) -> Res<u64> {
    let size = if table.wide { 8 } else { 4 };
    match c.rd.seek(table.pos as u64 + index * size)?.ptr()? {
        0 => Err("missing tile".into()),
        p => Ok(p),
    }
}

fn decode(c: &Ctx, at: u64, out: &mut [u8], bpp: usize) -> Res<()> {
    let start = c.rd.seek(at)?.pos;
    // GIMP's own cap on stored tile size, allowing for negative compression.
    let limit = (TILE * TILE) as usize * bpp * 3 / 2;
    let data = &c.rd.b[start..c.rd.b.len().min(start + limit)];
    match c.comp {
        0 => out.copy_from_slice(data.get(..out.len()).ok_or("truncated tile")?),
        1 => rle(data, out, bpp)?,
        2 => {
            let mut z = Decompress::new(true);
            z.decompress(data, out, FlushDecompress::Finish)
                .map_err(|e| format!("bad zlib tile: {e}"))?;
            if z.total_out() != out.len() as u64 {
                return Err("short zlib tile".into());
            }
        }
        n => return Err(format!("unsupported compression {n}")),
    }
    Ok(())
}

/// XCF RLE: each byte of the pixel is a separate plane, coded as runs and
/// literal spans, with a 16-bit length escape for spans of 128 or more.
fn rle(mut d: &[u8], out: &mut [u8], bpp: usize) -> Res<()> {
    const TRUNC: &str = "truncated RLE tile";
    let n = out.len() / bpp;
    for plane in 0..bpp {
        let mut i = 0;
        while i < n {
            let (&op, rest) = d.split_first().ok_or(TRUNC)?;
            d = rest;
            let literal = op >= 128;
            let mut len = if literal { 256 - op as usize } else { op as usize + 1 };
            if len == 128 {
                let (l, rest) = d.split_at_checked(2).ok_or(TRUNC)?;
                len = usize::from(u16::from_be_bytes([l[0], l[1]]));
                d = rest;
            }
            if i + len > n {
                return Err("RLE run overflows its tile".into());
            }
            let dst = out[(i * bpp + plane)..].iter_mut().step_by(bpp).take(len);
            if literal {
                let (src, rest) = d.split_at_checked(len).ok_or(TRUNC)?;
                dst.zip(src).for_each(|(o, &v)| *o = v);
                d = rest;
            } else {
                let (&v, rest) = d.split_first().ok_or(TRUNC)?;
                dst.for_each(|o| *o = v);
                d = rest;
            }
            i += len;
        }
    }
    Ok(())
}

/// One component scaled to 0..=1 (floats clamped, NaN as 0).
fn sample(p: &[u8], prec: Precision, le: bool) -> f32 {
    let n = prec.bytes();
    let mut b = [0u8; 8];
    b[..n].copy_from_slice(&p[..n]);
    if le {
        b[..n].reverse();
    }
    let raw = u64::from_be_bytes(b) >> (64 - 8 * n);
    let v = match prec.depth {
        Depth::U8 => raw as f32 / 255.0,
        Depth::U16 => raw as f32 / 65535.0,
        Depth::U32 => (raw as f64 / f64::from(u32::MAX)) as f32,
        Depth::Half => half(raw as u16),
        Depth::Float => f32::from_bits(raw as u32),
        Depth::Double => f64::from_bits(raw) as f32,
    };
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

fn half(h: u16) -> f32 {
    let sign = if h >> 15 == 1 { -1.0 } else { 1.0 };
    let (exp, man) = ((h >> 10) & 0x1f, f32::from(h & 0x3ff));
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 if man == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        e => (1.0 + man / 1024.0) * 2f32.powi(i32::from(e) - 15),
    }
}

/// Gamma-encoded colour and straight alpha of one stored pixel.
fn pixel(c: &Ctx, family: Family, prec: Precision, channels: usize, p: &[u8]) -> ([f32; 3], f32) {
    let bpc = prec.bytes();
    let s = |i: usize| sample(&p[i * bpc..], prec, c.le);
    let alpha = |i: usize| if channels > i { s(i) } else { 1.0 };
    let (rgb, a) = match family {
        Family::Indexed => {
            let [r, g, b] = c.cmap.get(usize::from(p[0])).copied().unwrap_or_default();
            return ([r, g, b].map(|v| f32::from(v) / 255.0), alpha(1));
        }
        Family::Gray => ([s(0); 3], alpha(1)),
        Family::Rgb => ([s(0), s(1), s(2)], alpha(3)),
    };
    // Alpha never carries a transfer curve; colour does in linear precisions.
    let lut = luts();
    let rgb = if prec.linear {
        rgb.map(|v| f32::from(lut.encode[(v * 65535.0 + 0.5) as usize]) / 255.0)
    } else {
        rgb
    };
    (rgb, a)
}

/// Straight-alpha "over" into an 8-bit sRGB canvas, mixing either in linear
/// light or directly on the gamma-encoded values.
fn over(d: &mut Rgba<u8>, s: [f32; 3], sa: f32, linear: bool) {
    let q = |v: f32| (v * 255.0 + 0.5) as u8;
    if sa <= 0.0 {
        return;
    }
    if sa >= 1.0 {
        *d = Rgba([q(s[0]), q(s[1]), q(s[2]), 255]);
        return;
    }
    let da = f32::from(d[3]) / 255.0;
    let oa = sa + da * (1.0 - sa);
    let (ws, wd) = (sa / oa, da * (1.0 - sa) / oa);
    let lut = luts();
    for k in 0..3 {
        d[k] = if linear {
            let v = lut.decode[(s[k] * 4095.0 + 0.5) as usize] * ws + lut.decode8[usize::from(d[k])] * wd;
            lut.encode[(v.min(1.0) * 65535.0 + 0.5) as usize]
        } else {
            q(s[k] * ws + f32::from(d[k]) / 255.0 * wd)
        };
    }
    d[3] = q(oa);
}

struct Luts {
    /// sRGB byte to linear.
    decode8: [f32; 256],
    /// Gamma-encoded 0..=1 at 12-bit resolution to linear.
    decode: Vec<f32>,
    /// Linear 0..=1 at 16-bit resolution to an sRGB byte; fine enough that
    /// shadows don't band.
    encode: Vec<u8>,
}

fn luts() -> &'static Luts {
    static L: OnceLock<Luts> = OnceLock::new();
    L.get_or_init(|| {
        let to_lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let to_srgb = |v: f32| {
            if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
        };
        Luts {
            decode8: std::array::from_fn(|i| to_lin(i as f32 / 255.0)),
            decode: (0..4096).map(|i| to_lin(i as f32 / 4095.0)).collect(),
            encode: (0..65536)
                .map(|i| (to_srgb(i as f32 / 65535.0) * 255.0 + 0.5) as u8)
                .collect(),
        }
    })
}
