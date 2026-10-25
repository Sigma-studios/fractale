//! Preview textures for the grid. Tiles ask for their preview while they're
//! drawn; the grid also asks for a screen ahead. Each frame the requests go
//! to the workers (`model::thumbs`), most wanted first, replacing the last
//! frame's, so scrolling past a thousand images doesn't queue a thousand
//! decodes. Finished previews become textures within a per-frame upload
//! budget, and the least recently drawn are dropped above a memory cap.

use crate::model::thumbs::cache::Bucket;
use crate::model::thumbs::{Key, Ready, Rendered, Request, Thumbs};
use eframe::egui::{self, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Smooth when shrinking, sharp pixels when enlarging (no blur on small
/// pictures and pixel art).
pub const UPSCALE: TextureOptions = TextureOptions {
    magnification: egui::TextureFilter::Nearest,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: None,
};

/// Texture memory kept for previews.
const MEMORY_CAP: usize = 256 << 20;
/// Pixels uploaded per frame at most (about 8 MB), so a screenful of
/// arriving previews doesn't stall a frame.
const UPLOAD_BUDGET: usize = 2 << 20;

enum Slot {
    /// Asked for, not back yet.
    Pending,
    /// Back, waiting for its turn to upload.
    Arrived,
    /// No preview possible: the type icon stays.
    Failed,
    Ready(Texture),
}

pub struct Texture {
    /// One frame, or an animation's frames with their durations.
    frames: Vec<(TextureHandle, Duration)>,
    pub size: egui::Vec2,
    bytes: usize,
    used: u64,
}

pub struct Previews {
    thumbs: Thumbs,
    slots: HashMap<Key, Slot>,
    arrived: VecDeque<(Key, Rendered)>,
    /// This frame's requests, most wanted first.
    wanted: Vec<Request>,
    sent: Vec<Key>,
    frame: u64,
    memory: usize,
    /// When animations started (they play from their first frame when they
    /// come on screen).
    epoch: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Unknown,
    Pending,
    Failed,
    Ready { frames: usize, size: egui::Vec2 },
}

/// What a tile draws.
pub struct Shown<'a> {
    pub texture: &'a TextureHandle,
    pub size: egui::Vec2,
    /// Repaint after this for the next animation frame.
    pub next: Option<Duration>,
}

impl Previews {
    pub fn new(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Previews {
        Previews {
            thumbs: Thumbs::spawn_with(root, crate::model::thumbs::previewers::registry, wake),
            slots: HashMap::new(),
            arrived: VecDeque::new(),
            wanted: Vec::new(),
            sent: Vec::new(),
            frame: 0,
            memory: 0,
            epoch: Instant::now(),
        }
    }

    pub fn key(path: &Path, mtime: i64, px: f32) -> Key {
        Key { path: path.to_path_buf(), mtime, bucket: Bucket::for_pixels(px) }
    }

    /// Ask for a preview (drawn or about to be). Pending ones are asked
    /// for again every frame they're still wanted.
    pub fn want(&mut self, key: Key, size: u64) {
        if matches!(self.slots.get(&key), None | Some(Slot::Pending)) {
            self.wanted.push(Request { key, size });
        }
    }

    /// The preview to draw for `key`: its own bucket, or meanwhile another
    /// bucket of the same file (resizing tiles doesn't flash icons). Asks
    /// for it if it isn't there.
    pub fn get(&mut self, key: &Key, size: u64) -> Option<Shown<'_>> {
        let frame = self.frame;
        let mut found = matches!(self.slots.get(key), Some(Slot::Ready(_))).then(|| key.clone());
        self.want(key.clone(), size);
        if found.is_none() {
            found = [Bucket::Normal, Bucket::Large, Bucket::XLarge, Bucket::XXLarge]
                .into_iter()
                .map(|bucket| Key { bucket, ..key.clone() })
                .find(|k| matches!(self.slots.get(k), Some(Slot::Ready(_))));
        }
        let Some(Slot::Ready(t)) = self.slots.get_mut(&found?) else { return None };
        t.used = frame;
        let (index, next) = t.frame_at(self.epoch.elapsed());
        Some(Shown { texture: &t.frames[index].0, size: t.size, next })
    }

    pub fn failed(&self, key: &Key) -> bool {
        matches!(self.slots.get(key), Some(Slot::Failed))
    }

    /// After the panes are drawn: send what's wanted, upload what arrived,
    /// stay under the memory cap.
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.thumbs.rx.try_recv() {
            match r {
                Ready::Done(key, rendered) => {
                    self.slots.insert(key.clone(), Slot::Arrived);
                    self.arrived.push_back((key, rendered));
                }
                Ready::Failed(key) => {
                    self.slots.insert(key, Slot::Failed);
                }
            }
        }
        let mut budget = UPLOAD_BUDGET;
        while budget > 0 {
            let Some((key, rendered)) = self.arrived.pop_front() else { break };
            let t = upload(ctx, &key, rendered, self.frame);
            budget = budget.saturating_sub(t.bytes / 4);
            self.memory += t.bytes;
            if let Some(Slot::Ready(old)) = self.slots.insert(key, Slot::Ready(t)) {
                self.memory -= old.bytes;
            }
        }
        if !self.arrived.is_empty() {
            ctx.request_repaint();
        }

        let wanted = std::mem::take(&mut self.wanted);
        let keys: Vec<Key> = wanted.iter().map(|r| r.key.clone()).collect();
        if keys != self.sent {
            // Anything wanted and not back yet stays pending; what's no
            // longer wanted is forgotten so it's asked for again later.
            let set: HashSet<&Key> = keys.iter().collect();
            self.slots.retain(|k, s| !matches!(s, Slot::Pending) || set.contains(k));
            for k in &keys {
                self.slots.entry(k.clone()).or_insert(Slot::Pending);
            }
            self.thumbs.want(wanted);
            self.sent = keys;
        }
        self.evict();
        self.frame += 1;
    }

    /// Drop the least recently drawn textures above the cap (never this
    /// frame's).
    fn evict(&mut self) {
        if self.memory <= MEMORY_CAP {
            return;
        }
        let mut ready: Vec<(u64, Key, usize)> = self
            .slots
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready(t) if t.used < self.frame => Some((t.used, k.clone(), t.bytes)),
                _ => None,
            })
            .collect();
        ready.sort_by_key(|r| r.0);
        for (_, k, bytes) in ready {
            if self.memory <= MEMORY_CAP * 3 / 4 {
                break;
            }
            self.slots.remove(&k);
            self.memory -= bytes;
        }
    }

    /// Where a file's preview is at (any bucket), for tests and stats.
    pub fn status(&self, path: &Path) -> Status {
        let mut out = Status::Unknown;
        for (_, s) in self.slots.iter().filter(|(k, _)| k.path == path) {
            out = match s {
                Slot::Ready(t) => return Status::Ready { frames: t.frames.len(), size: t.size },
                Slot::Failed => Status::Failed,
                Slot::Pending | Slot::Arrived if out == Status::Unknown => Status::Pending,
                _ => out,
            };
        }
        out
    }

    pub fn memory(&self) -> usize {
        self.memory
    }

    pub fn textures(&self) -> usize {
        self.slots.values().filter(|s| matches!(s, Slot::Ready(_))).count()
    }

    pub fn pending(&self) -> usize {
        self.slots.values().filter(|s| matches!(s, Slot::Pending | Slot::Arrived)).count()
    }
}

impl Texture {
    /// Which frame shows now, and how long until the next.
    fn frame_at(&self, t: Duration) -> (usize, Option<Duration>) {
        if self.frames.len() < 2 {
            return (0, None);
        }
        let total: Duration = self.frames.iter().map(|f| f.1).sum();
        let mut at = Duration::from_nanos((t.as_nanos() % total.as_nanos().max(1)) as u64);
        for (i, (_, d)) in self.frames.iter().enumerate() {
            if at < *d {
                return (i, Some(*d - at));
            }
            at -= *d;
        }
        (0, Some(self.frames[0].1))
    }
}

fn upload(ctx: &egui::Context, key: &Key, r: Rendered, frame: u64) -> Texture {
    let options = UPSCALE;
    let name = key.path.to_string_lossy();
    let frames: Vec<(image::RgbaImage, Duration)> = match r {
        Rendered::Still(i) => vec![(i, Duration::ZERO)],
        Rendered::Animated(f) => f,
    };
    let size = egui::vec2(frames[0].0.width() as f32, frames[0].0.height() as f32);
    let mut bytes = 0;
    let frames = frames
        .into_iter()
        .map(|(img, d)| {
            bytes += img.len();
            let c = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
            (ctx.load_texture(name.as_ref(), c, options), d)
        })
        .collect();
    Texture { frames, size, bytes, used: frame }
}
