//! A document's first lines drawn on a white page, for files whose layout
//! Fractale can't render itself (Word documents without a preview, HTML).
//! The page is an SVG drawn with resvg, so text gets real fonts.

use resvg::{tiny_skia, usvg};

const W: f32 = 210.0;
const H: f32 = 297.0;
const FONTS: &str = "DejaVu Sans, Liberation Sans, Noto Sans, Helvetica, Arial, sans-serif";

/// An A4 page: an optional bold title, then the text, wrapped.
pub fn render(title: Option<&str>, text: &str, max: u32) -> Result<image::RgbaImage, String> {
    // Large enough to read a few words in a tile.
    const SIZE: f32 = 8.0;
    const LINE: f32 = 11.0;
    const CHARS: usize = 40;
    let mut svg = format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}"><rect width="{W}" height="{H}" fill="#ffffff"/><g font-family="{FONTS}" fill="#2a2a28">"##);
    let mut y = 26.0;
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        for l in wrap(t, 26).into_iter().take(3) {
            svg.push_str(&format!(r#"<text x="16" y="{y}" font-size="12" font-weight="bold">{}</text>"#, escape(&l)));
            y += 15.0;
        }
        y += 6.0;
    }
    for para in text.lines().filter(|l| !l.trim().is_empty()) {
        for l in wrap(para, CHARS) {
            if y > H - 14.0 {
                break;
            }
            svg.push_str(&format!(r#"<text x="16" y="{y}" font-size="{SIZE}">{}</text>"#, escape(&l)));
            y += LINE;
        }
        y += 3.0;
    }
    svg.push_str("</g></svg>");
    let tree = usvg::Tree::from_str(&svg, super::svg::options()).map_err(|e| e.to_string())?;
    let k = max as f32 / H;
    let (w, h) = (((W * k).round() as u32).max(1), ((H * k).round() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("empty page")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pixmap.as_mut());
    // Opaque page: premultiplied is straight.
    image::RgbaImage::from_raw(w, h, pixmap.take()).ok_or_else(|| "bad size".into())
}

fn wrap(para: &str, chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in para.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + word.chars().count() + 1 > chars {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// XML/HTML character references (the common named ones and numeric).
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e < 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "rsquo" | "lsquo" => Some('\''),
            "rdquo" | "ldquo" => Some('"'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "eacute" => Some('é'),
            "egrave" => Some('è'),
            "agrave" => Some('à'),
            "ccedil" => Some('ç'),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}
