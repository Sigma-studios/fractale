//! Opening files with other applications, on a worker thread (finding a
//! file's type reads its first bytes; the app database is read once at
//! startup). The default app comes from the desktop's `mimeapps.list`
//! (see `apps`); programs aren't run without asking; with no database
//! (macOS) the platform's opener does it.

use super::apps::{self, Apps, MimeDb};
use super::platform;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub enum Req {
    /// With each file's default app.
    Open(Vec<PathBuf>),
    /// A program the user agreed to run.
    Run(PathBuf),
    /// With a chosen app (its desktop id).
    With { app: String, files: Vec<PathBuf> },
    /// "Other…": a typed command line.
    Custom { line: String, files: Vec<PathBuf> },
    /// The apps that can open these files (for "Open with").
    AppsFor(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppChoice {
    pub id: String,
    pub name: String,
    pub default: bool,
}

pub enum Reply {
    /// A program: run it? (ask first)
    AskRun(PathBuf),
    Apps { files: Vec<PathBuf>, mime: String, apps: Vec<AppChoice> },
    Failed(String),
}

pub struct Opener {
    tx: Sender<Req>,
    pub rx: Receiver<Reply>,
}

impl Opener {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> Opener {
        let (tx, jobs) = channel::<Req>();
        let (out, rx) = channel();
        std::thread::Builder::new()
            .name("opener".into())
            .spawn(move || {
                let db = MimeDb::from_env();
                let apps = Apps::from_env();
                while let Ok(req) = jobs.recv() {
                    for r in serve(&db, &apps, req) {
                        let _ = out.send(r);
                        wake();
                    }
                }
            })
            .expect("spawn opener");
        Opener { tx, rx }
    }

    pub fn send(&self, r: Req) {
        let _ = self.tx.send(r);
    }
}

fn serve(db: &MimeDb, apps: &Apps, req: Req) -> Vec<Reply> {
    let mut out = Vec::new();
    match req {
        Req::Open(files) => {
            // Programs ask; the rest go to their default app, grouped so an
            // app gets all its files at once.
            let mut by_app: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
            for f in files {
                let mime = db.type_of(&f);
                if apps::is_executable(&f, &mime, db) {
                    out.push(Reply::AskRun(f));
                    continue;
                }
                match apps.default_for(&mime, db) {
                    Some(a) => by_app.entry(a.id.clone()).or_default().push(f),
                    // No default known (or no database, macOS): the
                    // platform decides.
                    None => platform::open_default(&f),
                }
            }
            for (id, files) in by_app {
                if let Some(a) = apps.get(&id) {
                    out.extend(launch(apps, a, &files));
                }
            }
        }
        Req::Run(path) => platform::spawn_detached(std::process::Command::new(&path).current_dir(path.parent().unwrap_or(std::path::Path::new("/")))),
        Req::With { app, files } => match apps.get(&app) {
            Some(a) => out.extend(launch(apps, a, &files)),
            None => out.push(Reply::Failed(format!("{app} isn't installed"))),
        },
        Req::Custom { line, files } => match apps::custom_command(&line, &files) {
            Ok(argv) => run(&argv),
            Err(e) => out.push(Reply::Failed(e)),
        },
        Req::AppsFor(files) => {
            let Some(first) = files.first() else { return out };
            let mime = db.type_of(first);
            let default = apps.default_for(&mime, db).map(|a| a.id.clone());
            let list = apps.for_type(&mime, db).into_iter().map(|a| AppChoice { id: a.id.clone(), name: a.name.clone(), default: Some(&a.id) == default.as_ref() }).collect();
            out.push(Reply::Apps { files, mime, apps: list });
        }
    }
    out
}

fn launch(apps: &Apps, a: &apps::App, files: &[PathBuf]) -> Vec<Reply> {
    match apps.launch_commands(a, files) {
        Ok(cmds) => {
            for argv in cmds {
                run(&argv);
            }
            Vec::new()
        }
        Err(e) => vec![Reply::Failed(format!("{}: {e}", a.name))],
    }
}

fn run(argv: &[String]) {
    if let Some((program, args)) = argv.split_first() {
        platform::spawn_detached(std::process::Command::new(program).args(args));
    }
}
