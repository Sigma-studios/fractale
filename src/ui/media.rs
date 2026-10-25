//! Rest the pointer on a video, an audio file or an Orchestre file for a
//! moment (200 ms, or `FRACTALE_HOVER_DELAY_MS`) and it plays (see `model::playback`); move away and it stops.
//! Videos play in the tile, with their sound; sounds show their progress under it.

use super::theme::*;
use crate::model::playback::{Kind, Playback};
use eframe::egui::{self, pos2, vec2, Rect, TextureHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_millis(200);

/// How long the pointer rests before playback: `FRACTALE_HOVER_DELAY_MS`,
/// otherwise `DELAY`.
fn delay() -> Duration {
    std::env::var("FRACTALE_HOVER_DELAY_MS").ok().and_then(|v| v.trim().parse().ok()).map(Duration::from_millis).unwrap_or(DELAY)
}

pub struct Media {
    /// The playable tile under the pointer, since when, and its pixel size.
    hovered: Option<(PathBuf, Instant, u32)>,
    /// Reported by tiles this frame.
    seen: Option<(PathBuf, u32)>,
    pub playing: Option<Playback>,
    texture: Option<TextureHandle>,
    wake: Arc<dyn Fn() + Send + Sync>,
    delay: Duration,
}

impl Media {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Media {
        Media { hovered: None, seen: None, playing: None, texture: None, wake, delay: delay() }
    }

    /// A tile is under the pointer (call every frame it is).
    pub fn hover(&mut self, path: &Path, px: u32) {
        if Kind::of(path).is_some() {
            self.seen = Some((path.to_path_buf(), px));
        }
    }

    /// A video frame has been shown (tests).
    pub fn has_frame(&self) -> bool {
        self.texture.is_some()
    }

    pub fn is_playing(&self, path: &Path) -> bool {
        self.playing.as_ref().is_some_and(|p| p.path == path)
    }

    /// A playing video's current frame in place of the tile's preview;
    /// false if this tile isn't playing a video (or has no frame yet: the
    /// still stays until it does).
    pub fn draw_video(&mut self, ctx: &egui::Context, p: &egui::Painter, path: &Path, rect: Rect) -> bool {
        let Some(play) = self.playing.as_ref().filter(|pl| pl.path == path && pl.kind == Kind::Video) else { return false };
        if let Some(f) = play.frame() {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
            match &mut self.texture {
                Some(t) if t.size() == [f.width as usize, f.height as usize] => t.set(img, super::previews::UPSCALE),
                _ => self.texture = Some(ctx.load_texture("hover-video", img, super::previews::UPSCALE)),
            }
        }
        let Some(t) = &self.texture else { return false };
        let natural = t.size_vec2() / ctx.pixels_per_point();
        let k = (rect.width() / natural.x).min(rect.height() / natural.y);
        let size = natural * k;
        let r = Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, rect.bottom() - size.y), size);
        p.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
        true
    }

    /// Over a playing sound's preview: a play mark, and how far along it is.
    pub fn draw_sound(&self, p: &egui::Painter, path: &Path, rect: Rect) {
        let Some(play) = self.playing.as_ref().filter(|pl| pl.path == path && pl.kind != Kind::Video) else { return };
        let c = rect.left_top() + vec2(14.0, 14.0);
        p.circle_filled(c, 10.0, BG.gamma_multiply(0.85));
        p.add(egui::Shape::convex_polygon(vec![c + vec2(-3.5, -5.0), c + vec2(5.5, 0.0), c + vec2(-3.5, 5.0)], TEXT, egui::Stroke::NONE));
        if let Some(f) = play.progress() {
            let bar = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 3.0), vec2(rect.width(), 3.0));
            p.rect_filled(bar, 1.5, TRACK);
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, 3.0)), 1.5, SIZE);
        }
    }

    /// After the tiles: start, stop, and keep frames coming.
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        let seen = self.seen.take();
        let same = matches!((&seen, &self.hovered), (Some((a, _)), Some((b, _, _))) if a == b);
        if !same {
            self.hovered = seen.map(|(p, px)| (p, Instant::now(), px));
            if self.playing.as_ref().is_some_and(|pl| Some(&pl.path) != self.hovered.as_ref().map(|h| &h.0)) {
                self.playing = None;
                self.texture = None;
            }
        }
        if let Some((path, since, px)) = &self.hovered {
            if self.playing.is_none() {
                let waited = since.elapsed();
                if waited >= self.delay {
                    if let Some(kind) = Kind::of(path) {
                        self.playing = Some(Playback::start(path.clone(), kind, *px, self.wake.clone()));
                    }
                } else {
                    ctx.request_repaint_after(self.delay - waited);
                }
            }
        }
        // Sounds don't wake the UI; their progress bar needs frames.
        if self.playing.as_ref().is_some_and(|p| p.kind != Kind::Video) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
