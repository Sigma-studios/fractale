//! Make a folder of test images for the preview benchmarks: mostly
//! photo-sized JPEGs, some PNGs, each different so previews are told apart.
//! `cargo run --release --example make_images -- DIR COUNT`.

use rayon::prelude::*;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("DIR"));
    let n: usize = args.next().and_then(|s| s.parse().ok()).expect("COUNT");
    std::fs::create_dir_all(&dir).unwrap();
    (0..n).into_par_iter().for_each(|i| {
        let png = i % 7 == 0;
        let (w, h) = if png { (640, 480) } else if i % 3 == 0 { (1080, 1440) } else { (1600, 1200) };
        let name = dir.join(format!("img-{i:05}.{}", if png { "png" } else { "jpg" }));
        if name.exists() {
            return;
        }
        let hue = (i as f32 * 0.618_034).fract();
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let ring = (((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt() * (6.0 + (i % 5) as f32 * 3.0)).fract();
            let [r, g, b] = rgb((hue + ring * 0.25).fract(), 0.55 + 0.3 * v, 0.35 + 0.5 * u);
            let noise = ((x.wrapping_mul(7919) ^ y.wrapping_mul(104_729) ^ i as u32) % 23) as u8;
            image::Rgb([r.saturating_add(noise), g.saturating_add(noise), b.saturating_add(noise)])
        });
        img.save(&name).unwrap();
    });
}

fn rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}
