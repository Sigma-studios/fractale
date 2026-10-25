//! Render previews the way the grid does and save them as PNGs, to look at
//! a previewer's output or time it: `cargo run --release --example preview
//! -- OUT_DIR SIZE FILE...`.

use fractale::model::thumbs::{previewers, FileInfo};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("OUT_DIR"));
    let size: u32 = args.next().and_then(|s| s.parse().ok()).expect("SIZE");
    std::fs::create_dir_all(&out).unwrap();
    let all = previewers::registry();
    for file in args.map(PathBuf::from) {
        let Ok(md) = std::fs::metadata(&file) else {
            println!("{}: missing", file.display());
            continue;
        };
        let info = FileInfo::new(file.clone(), md.len(), md.mtime());
        let Some(p) = all.iter().find(|p| p.accepts(&info)) else {
            println!("{}: no previewer", file.display());
            continue;
        };
        let t = Instant::now();
        match p.render(&info, size) {
            Ok(r) => {
                let img = r.still();
                let name = out.join(format!("{}.png", file.file_name().unwrap().to_string_lossy()));
                img.save(&name).unwrap();
                println!("{}: {} {}×{} in {:.1?} -> {}", file.display(), p.name(), img.width(), img.height(), t.elapsed(), name.display());
            }
            Err(e) => println!("{}: {} failed in {:.1?}: {e}", file.display(), p.name(), t.elapsed()),
        }
    }
}
