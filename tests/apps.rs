//! Integration test against this machine's real freedesktop databases,
//! compared with the reference tools: `kmimetypefinder5` / `ktraderclient5`
//! (KDE Frameworks: QMimeDatabase and KService, what Dolphin uses), `gio`
//! (GLib's xdgmime, what GNOME uses), `xdg-mime` (which on a non-KDE/GNOME
//! session runs `mimetype`, File::MimeInfo, another shared-mime-info
//! implementation) and `file` (libmagic, an unrelated database, shown for
//! context only). Missing tools show up as "?".
//!
//! Skips itself when there is no shared-mime-info database (macOS).
//! Run with `--nocapture` to see the comparison tables.

use fractale::model::apps::{custom_command, is_executable, xdg_data_dirs, Apps, MimeDb};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn run(cmd: &str, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn kde_type(path: &Path) -> String {
    // kmimetypefinder5 prints nothing (and exits 1) for the default type.
    match run("kmimetypefinder5", &[&path.to_string_lossy()], &[]) {
        Some(s) if s.is_empty() => "application/octet-stream".into(),
        Some(s) => s,
        None => "?".into(),
    }
}

fn gio_type(path: &Path) -> String {
    run(
        "gio",
        &[
            "info",
            "-a",
            "standard::content-type",
            &path.to_string_lossy(),
        ],
        &[],
    )
    .and_then(|o| {
        o.lines().find_map(|l| {
            l.trim()
                .strip_prefix("standard::content-type: ")
                .map(str::to_string)
        })
    })
    .unwrap_or_else(|| "?".into())
}

/// One type per line for many files at once (`mimetype` and `file` both
/// support that), in batches to stay under the argv limit.
fn batch_types(cmd: &str, flags: &[&str], paths: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in paths.chunks(200) {
        let mut args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        args.extend(chunk.iter().map(|p| p.to_string_lossy().into_owned()));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = run(cmd, &refs, &[]).unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
        lines.resize(chunk.len(), "?".into());
        out.extend(lines);
    }
    out
}

struct Samples {
    dir: PathBuf,
}

impl Drop for Samples {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Synthetic files covering the interesting cases, plus real system files.
fn samples() -> (Samples, Vec<PathBuf>) {
    let dir = std::env::temp_dir().join(format!("fractale-apps-system-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let png =
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00";
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("image.png", png.to_vec()),
        ("png-without-extension", png.to_vec()),
        ("IMAGE.PNG", png.to_vec()),
        (
            "photo.jpg",
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01".to_vec(),
        ),
        ("anim.gif", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec()),
        (
            "doc.pdf",
            b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n".to_vec(),
        ),
        (
            "pdf-without-extension",
            b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n".to_vec(),
        ),
        (
            "page.html",
            b"<!DOCTYPE html>\n<html><body>hi</body></html>\n".to_vec(),
        ),
        (
            "html-without-extension",
            b"<!DOCTYPE html>\n<html><body>hi</body></html>\n".to_vec(),
        ),
        (
            "drawing.svg",
            b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n".to_vec(),
        ),
        ("main.rs", b"fn main() {}\n".to_vec()),
        ("main.c", b"int main(void) { return 0; }\n".to_vec()),
        ("main.C", b"int main() { return 0; }\n".to_vec()),
        ("Makefile", b"all:\n\ttrue\n".to_vec()),
        ("README", b"Read me.\n".to_vec()),
        ("README.md", b"# Title\n".to_vec()),
        (
            "archive.tar.gz",
            b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03".to_vec(),
        ),
        (
            "file.gz",
            b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03".to_vec(),
        ),
        (
            "archive.zip",
            b"PK\x03\x04\x14\x00\x00\x00\x08\x00".to_vec(),
        ),
        ("song.mp3", b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec()),
        (
            "sound.ogg",
            b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
        ),
        (
            "typescript.ts",
            b"const x: number = 1;\nexport default x;\n".to_vec(),
        ),
        ("video.ts", [0x47u8, 0x40, 0x00, 0x10].repeat(188).to_vec()),
        ("script-without-extension", b"#!/bin/sh\necho hi\n".to_vec()),
        (
            "script.py",
            b"#!/usr/bin/env python3\nprint('hi')\n".to_vec(),
        ),
        (
            "python-without-extension",
            b"#!/usr/bin/env python3\nprint('hi')\n".to_vec(),
        ),
        ("empty", Vec::new()),
        ("empty.txt", Vec::new()),
        (
            "notes-without-extension",
            "Plain words, accentués.\n".as_bytes().to_vec(),
        ),
        (
            "binary-without-extension",
            b"\x00\x01\x02\x03\x04\x05\xfe\xff".to_vec(),
        ),
        ("data.json", b"{\"a\": 1}\n".to_vec()),
        ("config.yaml", b"a: 1\n".to_vec()),
        ("Cargo.toml", b"[package]\nname = \"x\"\n".to_vec()),
        ("backup.txt~", b"old\n".to_vec()),
        ("libfoo.so.1.2", b"\x7fELF\x02\x01\x01\x00".to_vec()),
        ("text.doc", b"This is not a Word file.\n".to_vec()),
        (
            "word.doc",
            b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00\x00".to_vec(),
        ),
    ];
    let mut paths = Vec::new();
    for (name, bytes) in files {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        paths.push(p);
    }
    fs::create_dir_all(dir.join("folder.png")).unwrap();
    paths.push(dir.join("folder.png"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("image.png"), dir.join("link-to-png")).unwrap();
        paths.push(dir.join("link-to-png"));
    }
    for real in [
        "/usr/bin/ls",
        "/usr/bin/xdg-mime",
        "/etc/passwd",
        "/etc/os-release",
        "/usr/share/mime/packages/freedesktop.org.xml",
        "/usr/share/mime/magic",
        "/usr/share/applications/mpv.desktop",
        "/dev/null",
    ] {
        if Path::new(real).exists() {
            paths.push(PathBuf::from(real));
        }
    }
    (Samples { dir }, paths)
}

/// A sample of real files from the system for a bulk comparison.
fn system_files(limit: usize) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, per_dir: usize, limit: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        let mut taken = 0;
        for p in entries {
            if out.len() >= limit {
                return;
            }
            let Ok(meta) = fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.is_dir() {
                walk(&p, out, per_dir, limit);
            } else if meta.is_file() && taken < per_dir && fs::File::open(&p).is_ok() {
                out.push(p);
                taken += 1;
            }
        }
    }
    let mut out = Vec::new();
    for root in [
        "/usr/share/doc",
        "/usr/share/icons",
        "/usr/share/pixmaps",
        "/usr/share/mime",
        "/usr/share/applications",
        "/usr/share/man",
        "/usr/share/perl",
        "/usr/share/python3",
        "/usr/lib/python3",
        "/usr/bin",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib/x86_64-linux-gnu",
        "/etc",
        "/usr/share/fonts",
        "/usr/share/locale",
        "/usr/share/zoneinfo",
        "/usr/include",
    ] {
        let before = out.len();
        walk(Path::new(root), &mut out, 6, before + limit / 12);
    }
    out.truncate(limit);
    out
}

#[test]
fn real_system() {
    if !Path::new("/usr/share/mime/globs2").exists() {
        eprintln!("no shared-mime-info database here; skipping");
        return;
    }

    // --- Load time -------------------------------------------------------
    let t = Instant::now();
    let db = MimeDb::from_env();
    let mime_time = t.elapsed();
    let t = Instant::now();
    let apps = Apps::from_env();
    let apps_time = t.elapsed();
    println!(
        "load: MimeDb {:.2} ms, Apps {:.2} ms ({} apps), data dirs {:?}",
        mime_time.as_secs_f64() * 1e3,
        apps_time.as_secs_f64() * 1e3,
        apps.all().len(),
        xdg_data_dirs()
    );
    let mut best = f64::MAX;
    for _ in 0..10 {
        let t = Instant::now();
        let db = MimeDb::from_env();
        let apps = Apps::from_env();
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        assert!(db.type_of_name("a.png").is_some() && !apps.all().is_empty());
    }
    println!("load (both, best of 10, warm cache): {best:.2} ms");

    // --- MIME types of curated samples ------------------------------------
    let (_guard, paths) = samples();
    let xdg: Vec<String> = paths
        .iter()
        .map(|p| {
            run(
                "xdg-mime",
                &["query", "filetype", &p.to_string_lossy()],
                &[],
            )
            .unwrap_or_else(|| "?".into())
        })
        .collect();
    let file = batch_types("file", &["--brief", "--dereference", "--mime-type"], &paths);
    println!(
        "\n(* = differs from KDE)\n {:<44} {:<30} {:<30} {:<30} {:<30} file(1)",
        "sample", "ours", "kde", "gio", "xdg-mime"
    );
    for (i, p) in paths.iter().enumerate() {
        let ours = db.type_of(p);
        let kde = kde_type(p);
        let gio = gio_type(p);
        let mark = if ours == kde { " " } else { "*" };
        let name = p
            .strip_prefix(&_guard.dir)
            .unwrap_or(p)
            .display()
            .to_string();
        println!(
            "{mark}{name:<44} {ours:<30} {kde:<30} {gio:<30} {:<30} {}",
            xdg[i], file[i]
        );
    }
    let by_name: BTreeMap<String, String> = paths
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                db.type_of(p),
            )
        })
        .collect();
    let expect = [
        ("image.png", "image/png"),
        ("png-without-extension", "image/png"),
        ("IMAGE.PNG", "image/png"),
        ("pdf-without-extension", "application/pdf"),
        ("main.rs", "text/rust"),
        ("main.C", "text/x-c++src"),
        ("main.c", "text/x-csrc"),
        ("archive.tar.gz", "application/x-compressed-tar"),
        ("script-without-extension", "application/x-shellscript"),
        ("empty", "application/x-zerosize"),
        ("empty.txt", "text/plain"),
        ("notes-without-extension", "text/plain"),
        ("binary-without-extension", "application/octet-stream"),
        ("folder.png", "inode/directory"),
        ("link-to-png", "image/png"),
        ("script.py", "text/x-python3"),
        ("text.doc", "application/msword"),
        ("word.doc", "application/msword"),
        ("null", "inode/chardevice"),
    ];
    for (name, mime) in expect {
        if let Some(ours) = by_name.get(name) {
            // shared-mime-info versions differ on rust's canonical name.
            assert!(
                db.is_a(ours, mime) || db.is_a(mime, ours),
                "{name}: ours {ours}, expected {mime}"
            );
        }
    }

    // --- Bulk comparison on real files -------------------------------------
    let real = system_files(1000);
    let t = Instant::now();
    let ours: Vec<String> = real.iter().map(|p| db.type_of(p)).collect();
    let per_file = t.elapsed().as_secs_f64() * 1e6 / real.len().max(1) as f64;
    let xdg_bulk = batch_types("mimetype", &["--brief", "--dereference"], &real);
    let (use_kde, use_gio) = (have("kmimetypefinder5"), have("gio"));
    let mut agree = [0usize; 3];
    let mut diffs: [BTreeMap<(String, String), Vec<String>>; 3] = Default::default();
    for (i, p) in real.iter().enumerate() {
        let kde = if use_kde { kde_type(p) } else { "?".into() };
        let gio = if use_gio { gio_type(p) } else { "?".into() };
        for (k, other) in [kde, gio, xdg_bulk[i].clone()].into_iter().enumerate() {
            if ours[i] == other || db.unalias(&other) == ours[i] {
                agree[k] += 1;
            } else {
                diffs[k]
                    .entry((ours[i].clone(), other))
                    .or_default()
                    .push(p.display().to_string());
            }
        }
    }
    let pct = |n: usize| 100.0 * n as f64 / real.len().max(1) as f64;
    println!(
        "\nbulk: {} real files, {:.1} µs per type_of; agreement: kde {} ({:.1}%), gio {} ({:.1}%), xdg-mime/mimetype {} ({:.1}%)",
        real.len(),
        per_file,
        agree[0],
        pct(agree[0]),
        agree[1],
        pct(agree[1]),
        agree[2],
        pct(agree[2])
    );
    for (label, d) in ["kde", "gio", "xdg-mime"].iter().zip(&diffs) {
        for ((ours, other), files) in d {
            println!(
                "  ours {ours} / {label} {other}: {} file(s), e.g. {}",
                files.len(),
                files[..files.len().min(2)].join(", ")
            );
        }
    }
    if use_kde && !real.is_empty() {
        assert!(pct(agree[0]) >= 95.0, "agreement with KDE below 95%");
    }

    // --- is_executable on real binaries -------------------------------------
    for (p, expected) in [
        ("/usr/bin/ls", true),
        ("/usr/bin/xdg-mime", true),
        ("/etc/passwd", false),
    ] {
        let p = Path::new(p);
        if p.exists() {
            assert_eq!(
                is_executable(p, &db.type_of(p), &db),
                expected,
                "{}",
                p.display()
            );
        }
    }
    // A shared library with the x bit set (as everything in /nix/store is).
    if let Some(lib) = [
        "/usr/lib/aarch64-linux-gnu/libz.so.1",
        "/usr/lib/x86_64-linux-gnu/libz.so.1",
    ]
    .iter()
    .map(Path::new)
    .find(|p| p.exists())
    {
        let copy = _guard.dir.join("libz-copy.so.1");
        fs::copy(lib, &copy).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&copy, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mime = db.type_of(&copy);
        println!(
            "\nlibz copy: {mime}, is_executable = {}",
            is_executable(&copy, &mime, &db)
        );
        assert!(!is_executable(&copy, &mime, &db));
    }
    if let Some(libc) = [
        "/usr/lib/aarch64-linux-gnu/libc.so.6",
        "/usr/lib/x86_64-linux-gnu/libc.so.6",
    ]
    .iter()
    .map(Path::new)
    .find(|p| p.exists())
    {
        // glibc's libc.so.6 is both a library and a program (it prints its
        // version), and it does carry a PT_INTERP.
        println!(
            "libc.so.6 is_executable = {}",
            is_executable(libc, &db.type_of(libc), &db)
        );
    }
    assert_eq!(
        custom_command("ls -l", &[PathBuf::from("/tmp")]).unwrap(),
        ["ls", "-l", "/tmp"]
    );

    // --- Default applications -----------------------------------------------
    compare_defaults(&db, None);
    // And with a user config exercising the precedence rules: the
    // desktop-specific file beats mimeapps.list, a missing first choice
    // falls through, and a Removed Association drops a desktop file's claim.
    let conf = _guard.dir.join("config");
    fs::create_dir_all(&conf).unwrap();
    fs::write(
        conf.join("mimeapps.list"),
        "[Default Applications]\nimage/png=gone.desktop;gimp.desktop;\nimage/gif=feh.desktop\ntext/plain=vim.desktop\n\
         [Added Associations]\ntext/x-csrc=vim.desktop;\n\
         [Removed Associations]\nvideo/mp4=mpv.desktop;\n",
    )
    .unwrap();
    fs::write(
        conf.join("x-test-mimeapps.list"),
        "[Default Applications]\nimage/gif=gimp.desktop\n",
    )
    .unwrap();
    fs::write(
        conf.join("kde-mimeapps.list"),
        "[Default Applications]\nimage/gif=gimp.desktop\n",
    )
    .unwrap();
    compare_defaults(&db, Some(&conf));
}

/// KDE's preference-ordered list of apps for a type (first = default).
fn kde_apps(mime: &str, env: &[(&str, &str)]) -> Vec<String> {
    run(
        "ktraderclient5",
        &["--mimetype", mime, "--servicetype", "Application"],
        env,
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| l.trim().strip_prefix("DesktopEntryPath : '"))
    .filter_map(|p| {
        Path::new(p.trim_end_matches('\''))
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    })
    .collect()
}

fn compare_defaults(db: &MimeDb, config_home: Option<&Path>) {
    let types = [
        "text/plain",
        "text/x-csrc",
        "text/rust",
        "text/markdown",
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/svg+xml",
        "video/mp4",
        "video/x-matroska",
        "audio/mpeg",
        "audio/flac",
        "application/pdf",
        "text/html",
        "application/x-shellscript",
        "application/octet-stream",
        "inode/directory",
        "x-scheme-handler/geo",
    ];
    let mut config_dirs = fractale::model::apps::xdg_config_dirs();
    let mut base: Vec<(&str, String)> = Vec::new();
    if let Some(c) = config_home {
        base.push(("XDG_CONFIG_HOME", c.display().to_string()));
        config_dirs[0] = c.to_path_buf();
    }
    let env_for = |desktop: &str| {
        let mut e = base.clone();
        e.push(("XDG_CURRENT_DESKTOP", desktop.to_string()));
        e
    };
    let (generic_env, kde_env) = (env_for("X-Test"), env_for("KDE"));
    let as_ref = |e: &Vec<(&'static str, String)>| {
        e.iter().map(|(k, v)| (*k, v.clone())).collect::<Vec<_>>()
    };
    let generic_env = as_ref(&generic_env);
    let kde_env = as_ref(&kde_env);
    let generic_ref: Vec<(&str, &str)> =
        generic_env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let kde_ref: Vec<(&str, &str)> = kde_env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    // KDE results come from a KDE session (kde-mimeapps.list), the GLib and
    // xdg-mime ones from a generic desktop "X-Test" (x-test-mimeapps.list),
    // so ours is computed for both.
    let ours_generic = Apps::load(&config_dirs, &xdg_data_dirs(), Some("X-Test"));
    let ours_kde = Apps::load(&config_dirs, &xdg_data_dirs(), Some("KDE"));
    println!(
        "\ndefaults ({}); * = ours differs from the reference in the same row\n {:<26} {:<27} {:<27} | {:<27} {:<27} {:<27}",
        config_home.map_or("system state".to_string(), |c| format!("XDG_CONFIG_HOME={}", c.display())),
        "type", "ours (KDE)", "ktraderclient5", "ours (X-Test)", "gio", "xdg-mime"
    );
    let mut lists = Vec::new();
    for t in types {
        let id = |a: Option<&fractale::model::apps::App>| {
            a.map(|a| a.id.clone()).unwrap_or_else(|| "-".into())
        };
        let ours_k = id(ours_kde.default_for(t, db));
        let ours_g = id(ours_generic.default_for(t, db));
        let kde_list = kde_apps(t, &kde_ref);
        let kde = kde_list.first().cloned().unwrap_or_else(|| "-".into());
        let gio = run("gio", &["mime", t], &generic_ref)
            .and_then(|o| {
                o.lines()
                    .find_map(|l| {
                        l.strip_prefix("Default application for “")
                            .map(|r| r.to_string())
                    })
                    .and_then(|r| r.split(": ").nth(1).map(str::to_string))
            })
            .unwrap_or_else(|| "-".into());
        let xdg = run("xdg-mime", &["query", "default", t], &generic_ref)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into());
        let m = |a: &str, b: &str| if a == b { ' ' } else { '*' };
        println!(
            "{}{t:<26} {ours_k:<27} {kde:<27} |{}{ours_g:<27} {gio:<27}{}{xdg:<27}",
            m(&ours_k, &kde),
            m(&ours_g, &gio),
            m(&ours_g, &xdg)
        );
        let ours_list: Vec<String> = ours_kde
            .for_type(t, db)
            .iter()
            .map(|a| a.id.clone())
            .collect();
        lists.push((t, ours_list, kde_list));
    }
    println!("\n\"Open with\" lists, ours (KDE session) vs ktraderclient5:");
    for (t, ours, kde) in lists {
        let mark = if ours == kde { ' ' } else { '*' };
        println!(
            "{mark}{t:<26} ours: {}\n {:<26} kde:  {}",
            ours.join(" "),
            "",
            kde.join(" ")
        );
    }
}
