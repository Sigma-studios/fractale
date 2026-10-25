//! Previews of Pixelle files (`.pxl`), the user's own pixel-art editor.
//!
//! A Pixelle project is a folder of RON text files, each starting with what it is:
//! `Project(..)` (the root, `project.pxl`, holding the one palette), `Sprite(..)` or
//! `Terrain(..)`. Pixels are rows of two hex characters per palette index, `..` for the
//! transparent index 0. Pixelle is private, so this parses the format on its own, reading only
//! the fields a preview needs and ignoring the rest, so newer files keep previewing.
//!
//! What is drawn:
//! - a sprite: its first frame that has any pixels (normally frame 0, the first of the sheet), as
//!   the game gets it — the exported layers composited, the topmost non-zero index winning, then
//!   turned by the frame's transform. Visibility is an editor-only toggle and is ignored, as
//!   Pixelle's own export ignores it;
//! - a terrain: every tile the file holds, in file (slot) order, in a near-square grid with a
//!   one-pixel gap. Terrain files embed their tiles; there is no map to draw;
//! - the project root: its palette as swatches, each section starting a new row.
//!
//! A sprite's or terrain's colours come from the nearest `project.pxl` above it. Without one
//! (or if it does not parse) the palette is DawnBringer 32 at indices 1–32, which is what every
//! new Pixelle project starts with, then a grey ramp for 33–255.

use super::super::{FileInfo, Previewer, Rendered};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use serde::Deserialize;

/// Largest picture drawn, per side, before scaling; also the largest preview returned.
const MAX_SIDE: u32 = 4096;
/// Larger files are not Pixelle art; refuse rather than parse them.
const MAX_BYTES: usize = 32 << 20;
/// How far up to look for the project root.
const MAX_ANCESTORS: usize = 64;
const ROOT: &str = "project.pxl";
/// Index 0 plus at most 255 colours: Pixelle's palette is indexed by a byte.
const PALETTE_LEN: usize = 256;

type Colour = [u8; 4];

/// Render a preview of the `.pxl` file at `path`, whose contents are `bytes`, fitting `max`.
pub struct Pixelle;

impl Previewer for Pixelle {
    fn name(&self) -> &'static str {
        "pixelle"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "pxl"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_BYTES as u64 {
            return Err("too large for a Pixelle file".into());
        }
        let bytes = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        render(&f.path, &bytes, max).map(Rendered::Still)
    }
}

pub fn render(path: &Path, bytes: &[u8], max: u32) -> Result<RgbaImage, String> {
    if max == 0 {
        return Err("max is 0".into());
    }
    let art = match parse(bytes)? {
        File::Project(m) => swatches(&sections(&m))?,
        File::Sprite(s) => to_rgba(&sprite(&s)?, &project_palette(path)),
        File::Terrain(t) => to_rgba(&terrain(&t)?, &project_palette(path)),
    };
    Ok(scale(&art, max.min(MAX_SIDE)))
}

// --- The format ----------------------------------------------------------------------------------

#[derive(Deserialize)]
enum File {
    Project(Manifest),
    Sprite(SpriteFile),
    Terrain(TerrainFile),
}

#[derive(Deserialize)]
struct Manifest {
    /// In index order: the first colour of the first section is index 1.
    #[serde(default)]
    palette: Vec<Section>,
}

#[derive(Deserialize)]
struct Section {
    /// `(name, "#rrggbb" or "#rrggbbaa")`.
    #[serde(default)]
    colours: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct SpriteFile {
    /// Bottom to top.
    #[serde(default)]
    layers: Vec<Layer>,
    #[serde(default)]
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
struct Layer {
    name: String,
    /// A layer that is not exported is a guide (a sketch, notes), not part of the art.
    #[serde(default = "yes")]
    export: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct Frame {
    w: u32,
    h: u32,
    #[serde(default)]
    transform: D4,
    /// Each layer's rows, by layer name; a layer with nothing in this frame is left out.
    #[serde(default)]
    pixels: BTreeMap<String, Vec<String>>,
}

/// Applied in the order transpose, flip x, flip y.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct D4 {
    transpose: bool,
    flip_x: bool,
    flip_y: bool,
}

#[derive(Deserialize)]
struct TerrainFile {
    /// The size of every tile, square.
    cell: u32,
    /// Slot name to rows, in slot order. The preset is not needed: the grid shows what is drawn.
    tiles: Ordered,
}

/// A RON map kept in file order, since slot order is the order an artist sees.
struct Ordered(Vec<(String, Vec<String>)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of tiles")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    out.push(entry);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

fn parse(bytes: &[u8]) -> Result<File, String> {
    if bytes.is_empty() {
        return Err("empty file".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err("too large for a Pixelle file".into());
    }
    // Pixelle's earlier format, a binary single-file project with the same extension.
    if bytes.starts_with(b"PXLL") {
        return Err("an old single-file Pixelle project".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not a text file".to_string())?;
    ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::UNWRAP_VARIANT_NEWTYPES)
        .from_str(text)
        .map_err(|e| format!("not a Pixelle file: {e}"))
}

// --- Pictures of palette indices -----------------------------------------------------------------

struct Pic {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Pic {
    fn new(w: u32, h: u32) -> Result<Pic, String> {
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(format!("{w}x{h} is outside 1x1..{MAX_SIDE}x{MAX_SIDE}"));
        }
        Ok(Pic { w, h, px: vec![0; (w * h) as usize] })
    }

    /// Draw `src` at `(x, y)`, skipping index 0 and anything off the edge: a layer whose size
    /// disagrees with its frame is drawn clipped, as Pixelle does.
    fn blit(&mut self, src: &Pic, x0: u32, y0: u32) {
        for y in 0..src.h.min(self.h.saturating_sub(y0)) {
            for x in 0..src.w.min(self.w.saturating_sub(x0)) {
                let v = src.px[(y * src.w + x) as usize];
                if v != 0 {
                    self.px[((y0 + y) * self.w + x0 + x) as usize] = v;
                }
            }
        }
    }

    fn transformed(self, t: D4) -> Pic {
        if !(t.transpose || t.flip_x || t.flip_y) {
            return self;
        }
        let (w, h) = if t.transpose { (self.h, self.w) } else { (self.w, self.h) };
        let mut px = vec![0; self.px.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                let (mut tx, mut ty) = if t.transpose { (y, x) } else { (x, y) };
                if t.flip_x {
                    tx = w - 1 - tx;
                }
                if t.flip_y {
                    ty = h - 1 - ty;
                }
                px[(ty * w + tx) as usize] = self.px[(y * self.w + x) as usize];
            }
        }
        Pic { w, h, px }
    }
}

/// Rows of two-character pixels: `..` is 0, anything else two hex digits.
fn decode(rows: &[String]) -> Result<Pic, String> {
    let chars = rows.first().map_or(0, |r| r.len());
    if !chars.is_multiple_of(2) {
        return Err(format!("a row of {chars} characters; a pixel is two"));
    }
    let mut pic = Pic::new(chars as u32 / 2, rows.len().min(u32::MAX as usize) as u32)?;
    for (y, row) in rows.iter().enumerate() {
        if row.len() != chars {
            return Err(format!("row {y} is {} characters long, row 0 is {chars}", row.len()));
        }
        for (x, pair) in row.as_bytes().chunks_exact(2).enumerate() {
            pic.px[y * pic.w as usize + x] = match (pair[0], pair[1]) {
                (b'.', b'.') => 0,
                (a, b) => match (hex(a), hex(b)) {
                    (Some(a), Some(b)) => a << 4 | b,
                    _ => return Err(format!("row {y}, pixel {x} is not a palette index")),
                },
            };
        }
    }
    Ok(pic)
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

fn sprite(s: &SpriteFile) -> Result<Pic, String> {
    let exported = || s.layers.iter().filter(|l| l.export);
    // Frame 0 is the sprite's first frame on its sheet. If it is blank (a sheet laid out from a
    // later frame), the first frame with art says more; checked on the text, without decoding.
    let has_art = |f: &Frame| {
        exported().any(|l| f.pixels.get(&l.name).is_some_and(|rows| rows.iter().any(|r| r.bytes().any(|b| b != b'.'))))
    };
    let frame = s.frames.iter().find(|f| has_art(f)).or(s.frames.first()).ok_or("a sprite with no frames")?;
    let mut out = Pic::new(frame.w, frame.h)?;
    for layer in exported() {
        if let Some(rows) = frame.pixels.get(&layer.name).filter(|r| !r.is_empty()) {
            out.blit(&decode(rows)?, 0, 0);
        }
    }
    Ok(out.transformed(frame.transform))
}

fn terrain(t: &TerrainFile) -> Result<Pic, String> {
    let n = t.tiles.0.len() as u64;
    if n == 0 {
        return Err("a terrain with no tiles".into());
    }
    if t.cell == 0 || t.cell > MAX_SIDE {
        return Err(format!("tiles of {} pixels", t.cell));
    }
    let cols = (1..=n).find(|c| c * c >= n).unwrap_or(n);
    let rows = n.div_ceil(cols);
    let pitch = t.cell as u64 + 1;
    let side = |k: u64| u32::try_from(k * pitch - 1).unwrap_or(u32::MAX);
    let mut out = Pic::new(side(cols), side(rows))?;
    for (i, (_, tile)) in t.tiles.0.iter().enumerate() {
        let (cx, cy) = (i as u64 % cols, i as u64 / cols);
        let tile = decode(tile)?;
        // A tile larger than its cell would spill into its neighbour; clip it to the cell.
        let mut cell = Pic::new(t.cell, t.cell)?;
        cell.blit(&tile, 0, 0);
        out.blit(&cell, (cx * pitch) as u32, (cy * pitch) as u32);
    }
    Ok(out)
}

// --- Colour --------------------------------------------------------------------------------------

/// Each section's colours; an unreadable colour keeps its index but draws transparent.
fn sections(m: &Manifest) -> Vec<Vec<Colour>> {
    let mut left = PALETTE_LEN - 1;
    m.palette
        .iter()
        .map(|s| {
            let take = s.colours.len().min(left);
            left -= take;
            s.colours[..take].iter().map(|(_, c)| colour(c).unwrap_or([0; 4])).collect()
        })
        .collect()
}

/// `#rrggbb` or `#rrggbbaa`.
fn colour(s: &str) -> Option<Colour> {
    let d = s.strip_prefix('#').unwrap_or(s).as_bytes();
    let byte = |i: usize| Some(hex(*d.get(i)?)? << 4 | hex(*d.get(i + 1)?)?);
    match d.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// The palette of the project the file belongs to, index 0 transparent.
fn project_palette(path: &Path) -> Vec<Colour> {
    let from_root = || {
        let root = find_root(path)?;
        if std::fs::metadata(&root).ok()?.len() > MAX_BYTES as u64 {
            return None;
        }
        match parse(&std::fs::read(&root).ok()?).ok()? {
            File::Project(m) => Some(std::iter::once([0; 4]).chain(sections(&m).into_iter().flatten()).collect()),
            _ => None,
        }
    };
    from_root().unwrap_or_else(fallback_palette)
}

/// The nearest `project.pxl` in the file's folder or above.
fn find_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.ancestors().take(MAX_ANCESTORS).map(|d| d.join(ROOT)).find(|p| p.is_file())
}

/// DawnBringer 32, which every new Pixelle project starts with at indices 1–32, then greys so
/// that higher indices still show shapes.
fn fallback_palette() -> Vec<Colour> {
    const DB32: [u32; 32] = [
        0x000000, 0x222034, 0x45283c, 0x663931, 0x8f563b, 0xdf7126, 0xd9a066, 0xeec39a, 0xfbf236, 0x99e550, 0x6abe30,
        0x37946e, 0x4b692f, 0x524b24, 0x323c39, 0x3f3f74, 0x306082, 0x5b6ee1, 0x639bff, 0x5fcde4, 0xcbdbfc, 0xffffff,
        0x9badb7, 0x847e87, 0x696a6a, 0x595652, 0x76428a, 0xac3232, 0xd95763, 0xd77bba, 0x8f974a, 0x8a6f30,
    ];
    let db32 = DB32.iter().map(|&v| [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]);
    let greys = (33..PALETTE_LEN).map(|i| {
        let g = ((i - 33) * 255 / (PALETTE_LEN - 34)) as u8;
        [g, g, g, 255]
    });
    std::iter::once([0; 4]).chain(db32).chain(greys).collect()
}

/// Indices past the palette draw transparent, as in Pixelle.
fn to_rgba(pic: &Pic, palette: &[Colour]) -> RgbaImage {
    RgbaImage::from_fn(pic.w, pic.h, |x, y| {
        Rgba(palette.get(pic.px[(y * pic.w + x) as usize] as usize).copied().unwrap_or([0; 4]))
    })
}

/// The palette as swatches of 3x3 pixels with a 1-pixel gap, each section starting a row, as
/// many columns as makes the grid closest to square.
fn swatches(sections: &[Vec<Colour>]) -> Result<RgbaImage, String> {
    const SWATCH: u32 = 3;
    const PITCH: u32 = SWATCH + 1;
    let rows_for = |cols: usize| sections.iter().map(|s| s.len().div_ceil(cols)).sum::<usize>();
    if rows_for(1) == 0 {
        return Err("an empty palette".into());
    }
    let squareness = |cols: usize| {
        let (a, b) = (cols.max(rows_for(cols)), cols.min(rows_for(cols)).max(1));
        a as f64 / b as f64
    };
    let widest = sections.iter().map(Vec::len).max().unwrap_or(1);
    let cols = [4, 8, 16, 32]
        .into_iter()
        .map(|c: usize| c.min(widest))
        .min_by(|a, b| squareness(*a).total_cmp(&squareness(*b)))
        .unwrap_or(16);
    let rows = rows_for(cols);
    let mut out = RgbaImage::new(cols as u32 * PITCH - 1, rows as u32 * PITCH - 1);
    let mut row = 0;
    for s in sections {
        for (i, c) in s.iter().enumerate() {
            let (x0, y0) = ((i % cols) as u32 * PITCH, (row + i / cols) as u32 * PITCH);
            for y in y0..y0 + SWATCH {
                for x in x0..x0 + SWATCH {
                    out.put_pixel(x, y, Rgba(*c));
                }
            }
        }
        row += s.len().div_ceil(cols);
    }
    Ok(out)
}

/// Up by the largest whole factor that fits `max`, or down to fit it, both nearest-neighbour so
/// pixels stay crisp.
fn scale(img: &RgbaImage, max: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let side = w.max(h).max(1);
    let (nw, nh) = if side <= max {
        let k = max / side;
        (w * k, h * k)
    } else {
        let fit = |v: u32| ((v as u64 * max as u64 + side as u64 / 2) / side as u64).clamp(1, max as u64) as u32;
        (fit(w), fit(h))
    };
    if (nw, nh) == (w, h) {
        return img.clone();
    }
    // The source pixel under each target pixel's centre.
    let src = |v: u32, n: u32, of: u32| ((2 * v as u64 + 1) * of as u64 / (2 * n as u64)) as u32;
    RgbaImage::from_fn(nw, nh, |x, y| *img.get_pixel(src(x, nw, w), src(y, nh, h)))
}
