use std::path::{Path, PathBuf};

use fractale::model::thumbs::previewers::pixelle::render;

/// Pixelle's repository (private) for its example art: `FRACTALE_PIXELLE`,
/// or next to this one. Tests that need it pass without it.
fn pixelle() -> Option<PathBuf> {
    let p = std::env::var_os("FRACTALE_PIXELLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../pixelle"));
    if p.join("examples").is_dir() {
        Some(p)
    } else {
        eprintln!("no Pixelle repository at {}: skipped", p.display());
        None
    }
}

fn pxl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && p.file_name().is_some_and(|n| n != "target") {
            pxl_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "pxl") {
            out.push(p);
        }
    }
}

fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn render_file(p: &Path, max: u32) -> Result<image::RgbaImage, String> {
    render(p, &std::fs::read(p).unwrap(), max)
}

fn rgba(img: &image::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    img.get_pixel(x, y).0
}

/// A scratch project folder, unique per test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fractale-pixelle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn every_example_renders() {
    let Some(root) = pixelle() else { return };
    let mut files = Vec::new();
    pxl_files(&root.join("examples"), &mut files);
    files.sort();
    assert!(files.len() >= 9, "found {files:?}");
    let out = out_dir();
    for p in &files {
        for max in [1, 7, 16, 64, 256, 300] {
            let img = render_file(p, max).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let (w, h) = img.dimensions();
            assert!(w >= 1 && h >= 1 && w.max(h) <= max, "{} at {max}: {w}x{h}", p.display());
            if max == 256 {
                assert!(img.pixels().any(|px| px.0[3] > 0), "{} is blank", p.display());
                let rel = p.strip_prefix(&root).unwrap().to_string_lossy().replace('/', "_");
                img.save(out.join(rel.replace(".pxl", ".png"))).unwrap();
                println!("{rel}: {w}x{h}");
            }
        }
    }
}

#[test]
fn a_sprite_is_frame_0_of_its_exported_layers_in_project_colours() {
    let Some(root) = pixelle() else { return };
    let p = root.join("examples/bevy-showcase/assets/run2d/player.pxl");
    // At its own size: 8x8, unscaled.
    let img = render_file(&p, 8).unwrap();
    assert_eq!(img.dimensions(), (8, 8));
    // Index 0x21 is the first colour of the second section ("hair", #222034), after DB32's 32.
    assert_eq!(rgba(&img, 3, 0), [0x22, 0x20, 0x34, 255]);
    // Index 0x25 is "skin", #eec39a.
    assert_eq!(rgba(&img, 3, 1), [0xee, 0xc3, 0x9a, 255]);
    // layer_2 is not exported (and hidden): its pixels at (6, 3) and (7, 3) do not show.
    assert_eq!(rgba(&img, 6, 3)[3], 0);
    assert_eq!(rgba(&img, 7, 3)[3], 0);
    // Scaled by the largest whole factor: 8 * 12 = 96 <= 100.
    let big = render_file(&p, 100).unwrap();
    assert_eq!(big.dimensions(), (96, 96));
    assert_eq!(rgba(&big, 3 * 12 + 11, 11), [0x22, 0x20, 0x34, 255]);
}

#[test]
fn old_binary_projects_are_errors() {
    let Some(root) = pixelle() else { return };
    let mut files = Vec::new();
    pxl_files(&root.join("out"), &mut files);
    for p in files {
        let e = render_file(&p, 128).unwrap_err();
        println!("{}: {e}", p.display());
    }
    assert!(render(Path::new("a.pxl"), b"PXLL\x02\0\0", 64).unwrap_err().contains("old"));
}

#[test]
fn garbage_is_an_error() {
    let p = Path::new("/nonexistent/x.pxl");
    for bytes in [
        &b""[..],
        b"\xff\xfe\x00garbage",
        b"hello world",
        b"Sprite(",
        b"Sprite(version: 1, layers: [], frames: [])",
        b"Palette(version: 1)",
        b"Terrain(version: 1, cell: 8, tiles: {})",
        b"Terrain(version: 1, cell: 0, tiles: {\"a\": [\"0101\"]})",
        b"Project(version: 1, name: \"x\", palette: [], files: [])",
        b"Sprite(layers: [(name: \"art\")], frames: [(w: 2, h: 1, pixels: {\"art\": [\"zz..\"]})])",
        b"Sprite(layers: [(name: \"art\")], frames: [(w: 2, h: 2, pixels: {\"art\": [\"0101\", \"01\"]})])",
        b"Sprite(layers: [(name: \"art\")], frames: [(w: 2, h: 1, pixels: {\"art\": [\"010\"]})])",
        b"Sprite(layers: [(name: \"art\")], frames: [(w: 0, h: 1)])",
        b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[",
    ] {
        let r = render(p, bytes, 64);
        assert!(r.is_err(), "{:?} rendered", String::from_utf8_lossy(bytes));
    }
    assert!(render(p, b"", 64).unwrap_err().contains("empty"));
    let deep = "[".repeat(100_000);
    assert!(render(p, deep.as_bytes(), 64).is_err());
}

#[test]
fn mangled_examples_never_panic() {
    let Some(root) = pixelle() else { return };
    let real = std::fs::read(root.join("examples/bevy-showcase/assets/run2d/walls.pxl")).unwrap();
    let p = Path::new("/nonexistent/walls.pxl");
    for cut in (0..real.len()).step_by(7) {
        let _ = render(p, &real[..cut], 64);
    }
    let mut seed = 1u32;
    for _ in 0..2000 {
        let mut b = real.clone();
        for _ in 0..3 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let i = (seed >> 8) as usize % b.len();
            let alphabet = b"0123456789abcdef.,:()[]{}\"#xz ";
            b[i] = alphabet[(seed >> 3) as usize % alphabet.len()];
        }
        if let Ok(img) = render(p, &b, 64) {
            assert!(img.width().max(img.height()) <= 64);
        }
    }
}

#[test]
fn oversized_sprites_are_errors() {
    let p = Path::new("/nonexistent/big.pxl");
    let declared = b"Sprite(layers: [(name: \"art\")], frames: [(w: 5000, h: 8)])";
    assert!(render(p, declared, 64).unwrap_err().contains("5000x8"));
    let huge = b"Sprite(layers: [(name: \"art\")], frames: [(w: 4294967295, h: 4294967295)])";
    assert!(render(p, huge, 64).is_err());
    // A layer wider than the limit, in a frame that is not.
    let row = "01".repeat(5000);
    let wide = format!("Sprite(layers: [(name: \"art\")], frames: [(w: 8, h: 1, pixels: {{\"art\": [\"{row}\"]}})])");
    assert!(render(p, wide.as_bytes(), 64).is_err());
    let cell = b"Terrain(version: 1, cell: 100000, tiles: {\"a\": [\"01\"]})";
    assert!(render(p, cell, 64).is_err());
    // At the limit is fine, and comes back scaled down.
    let ok = b"Sprite(layers: [(name: \"art\")], frames: [(w: 4096, h: 2, pixels: {\"art\": [\"01\"]})])";
    assert_eq!(render(p, ok, 64).unwrap().dimensions(), (64, 1));
}

#[test]
fn palette_comes_from_the_nearest_project_or_falls_back() {
    let dir = scratch("palette");
    let sprite = "Sprite(\n  version: 1,\n  layers: [(name: \"art\"), (name: \"guide\", export: false)],\n  frames: [(w: 3, h: 1, transform: (transpose: true), pixels: {\"art\": [\"0102..\"], \"guide\": [\"......03\"]}), (w: 1, h: 1)],\n  newer_field: Something(1),\n)\n";
    let project = "Project(version: 1, name: \"t\", palette: [(label: \"a\", colours: [(\"red\", \"#ff0000\")]), (label: \"b\", colours: [(\"glass\", \"#0000ff80\"), (\"bad\", \"nope\")])], files: [])";
    std::fs::create_dir_all(dir.join("deep/er")).unwrap();
    std::fs::write(dir.join("project.pxl"), project).unwrap();
    let file = dir.join("deep/er/s.pxl");
    let img = render(&file, sprite.as_bytes(), 3).unwrap();
    // Transposed: 3x1 becomes 1x3.
    assert_eq!(img.dimensions(), (1, 3));
    assert_eq!(rgba(&img, 0, 0), [255, 0, 0, 255]);
    assert_eq!(rgba(&img, 0, 1), [0, 0, 255, 0x80]);
    assert_eq!(rgba(&img, 0, 2), [0; 4]);

    // A nearer project wins.
    std::fs::write(dir.join("deep/project.pxl"), project.replace("#ff0000", "#00ff00")).unwrap();
    assert_eq!(rgba(&render(&file, sprite.as_bytes(), 3).unwrap(), 0, 0), [0, 255, 0, 255]);

    // No project: DawnBringer 32 for 1–32. Index 1 is black, 2 is #222034.
    let lone = scratch("lone").join("s.pxl");
    let img = render(&lone, sprite.as_bytes(), 3).unwrap();
    assert_eq!(rgba(&img, 0, 0), [0, 0, 0, 255]);
    assert_eq!(rgba(&img, 0, 1), [0x22, 0x20, 0x34, 255]);

    // The project's own swatches: three colours, the unreadable one transparent.
    let img = render(&dir.join("project.pxl"), project.as_bytes(), 1000).unwrap();
    assert!(img.width().max(img.height()) <= 1000);
    assert!(img.pixels().any(|p| p.0 == [255, 0, 0, 255]) && img.pixels().any(|p| p.0 == [0, 0, 255, 0x80]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_blank_frame_0_gives_way_to_the_first_with_art() {
    let s = b"Sprite(layers: [(name: \"art\")], frames: [(w: 2, h: 2), (w: 2, h: 1, pixels: {\"art\": [\"0101\"]})])";
    assert_eq!(render(Path::new("/nonexistent/s.pxl"), s, 2).unwrap().dimensions(), (2, 1));
}

#[test]
fn shrinking_keeps_the_aspect() {
    let row = "01".repeat(300);
    let rows = vec![format!("\"{row}\""); 10].join(",");
    let s = format!("Sprite(layers: [(name: \"art\")], frames: [(w: 300, h: 10, pixels: {{\"art\": [{rows}]}})])");
    assert_eq!(render(Path::new("/nonexistent/s.pxl"), s.as_bytes(), 100).unwrap().dimensions(), (100, 3));
    assert!(render(Path::new("/nonexistent/s.pxl"), s.as_bytes(), 0).is_err());
}
