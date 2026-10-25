//! Orchestre files: songs (`.orch`) as a piano roll, one lane per track in
//! the track's colour, the way the editor shows them; sound effects
//! (`.orsfx`) as the waveform of one play, rendered with Orchestre's own
//! synth (`orchestre-dsp`), so the picture is what the game will play.

use super::super::{FileInfo, Previewer, Rendered};
use orchestre_core::Project;
use std::time::{Duration, Instant};

/// Song files are JSON; anything bigger is not a song.
const MAX_FILE: u64 = 32 << 20;
/// One play is at most this long in the preview, and rendering it at most
/// this slow (looping sounds are released after their preview length).
const MAX_SOUND: f32 = 10.0;
const BUDGET: Duration = Duration::from_secs(4);
const SAMPLE_RATE: u32 = 22050;

const BACKGROUND: [u8; 4] = [0x1a, 0x1a, 0x19, 0xff];
const LANE: [u8; 4] = [0x22, 0x22, 0x20, 0xff];

pub struct Orchestre;

impl Previewer for Orchestre {
    fn name(&self) -> &'static str {
        "orchestre"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "orch" || f.ext == "orsfx"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large for a song".into());
        }
        let bytes = std::fs::read(&f.path).map_err(|e| e.to_string())?;
        let img = if f.ext == "orch" {
            piano_roll(&orchestre_core::file::parse_project_bytes(&bytes)?, max)
        } else {
            let sound = orchestre_core::sfx::file::parse_sound_bytes(&bytes)?;
            super::audio::draw(&sound_levels(&sound)?, max, super::audio::COLOR)
        };
        Ok(Rendered::Still(img))
    }
}

/// Peak per 512-sample chunk of one play, released and cut like
/// `orchestre_dsp::render_sound`, but bounded in length and time (a hostile
/// file mustn't keep a worker forever).
fn sound_levels(sound: &orchestre_core::sfx::Sound) -> Result<Vec<f32>, String> {
    let start = Instant::now();
    let mut player = orchestre_dsp::SoundPlayer::new(sound, &orchestre_core::sfx::PlayOpts::default(), SAMPLE_RATE as f32);
    let release_at = sound.loop_preview().map(|s| (s.min(MAX_SOUND) * SAMPLE_RATE as f32) as usize);
    let chunk = 512;
    let (mut l, mut r) = (vec![0f32; chunk], vec![0f32; chunk]);
    let mut levels = Vec::new();
    let mut done = 0usize;
    while !player.is_done() && done < (MAX_SOUND * SAMPLE_RATE as f32) as usize {
        if start.elapsed() > BUDGET {
            return Err("rendering took too long".into());
        }
        if release_at.is_some_and(|at| done >= at) {
            player.release();
        }
        player.process(&mut l, &mut r);
        levels.push(l.iter().chain(&r).fold(0f32, |m, s| m.max(s.abs())));
        done += chunk;
    }
    // Drop the silent end, as Orchestre does.
    let end = levels.iter().rposition(|&v| v > 1e-4).map_or(0, |i| i + 1);
    levels.truncate(end);
    if levels.is_empty() {
        return Err("silent".into());
    }
    Ok(levels)
}

/// Lanes for the tracks that have notes, each as tall as its pitch range
/// (with a minimum, so a bass line still shows), notes across the song.
fn piano_roll(p: &Project, max: u32) -> image::RgbaImage {
    let (w, h) = (max.max(16), (max * 3 / 4).max(12));
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba(BACKGROUND));
    let tracks: Vec<_> = p.tracks.iter().filter(|t| !t.notes.is_empty()).collect();
    if tracks.is_empty() {
        return img;
    }
    let end = p.content_end().max(1) as f64;
    let pad = (max / 32).max(1);
    let ranges: Vec<(u8, u8)> = tracks
        .iter()
        .map(|t| {
            let lo = t.notes.iter().map(|n| n.pitch).min().unwrap_or(0);
            let hi = t.notes.iter().map(|n| n.pitch).max().unwrap_or(0);
            (lo, hi)
        })
        .collect();
    let weight = |(lo, hi): (u8, u8)| (hi - lo) as f64 + 6.0;
    let total: f64 = ranges.iter().map(|&r| weight(r)).sum();
    let gap = 1;
    let usable = h.saturating_sub(2 * pad + gap * (tracks.len() as u32 - 1)) as f64;
    let mut y = pad as f64;
    for (t, &(lo, hi)) in tracks.iter().zip(&ranges) {
        let lane_h = usable * weight((lo, hi)) / total;
        let (top, bottom) = (y.round() as u32, (y + lane_h).round() as u32);
        fill(&mut img, pad, top, w - pad, bottom, LANE);
        // Pitches spread over the lane, the highest at the top.
        let rows = (hi - lo) as f64 + 1.0;
        let note_h = (lane_h / rows).clamp(1.0, lane_h / 2.0).max(1.0);
        for n in &t.notes {
            let x0 = pad as f64 + (n.start as f64 / end) * (w - 2 * pad) as f64;
            let x1 = pad as f64 + ((n.start + n.len.max(1)) as f64 / end) * (w - 2 * pad) as f64;
            let row = (hi - n.pitch) as f64;
            let ny = y + (lane_h - note_h) * if rows > 1.0 { row / (rows - 1.0) } else { 0.5 };
            let a = (140.0 + 115.0 * n.vel.clamp(0.0, 1.0)) as u8;
            let [r, g, b] = t.color;
            fill(&mut img, x0 as u32, ny as u32, (x1.max(x0 + 1.0)) as u32, (ny + note_h).ceil() as u32, [r, g, b, a]);
        }
        y += lane_h + gap as f64;
    }
    img
}

/// Fill a rectangle, blending `rgba` over what's there.
fn fill(img: &mut image::RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, rgba: [u8; 4]) {
    let a = rgba[3] as u32;
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            let p = img.get_pixel_mut(x, y);
            for c in 0..3 {
                p[c] = ((rgba[c] as u32 * a + p[c] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = concat!(env!("HOME"), "/Projects/orchestre/crates/bevy/assets/music/demo.orch");

    #[test]
    fn song_and_sound() {
        // Orchestre's demo song, when the repository is next to this one.
        if std::path::Path::new(DEMO).exists() {
            let md = std::fs::metadata(DEMO).unwrap();
            let img = Orchestre.render(&FileInfo::new(DEMO.into(), md.len(), 0), 256).unwrap();
            let img = img.still();
            assert_eq!((img.width(), img.height()), (256, 192));
            let coloured = img.pixels().filter(|p| p.0 != BACKGROUND && p.0 != LANE).count();
            assert!(coloured > 500, "notes drawn: {coloured}");
        }
        let dir = std::env::temp_dir().join(format!("fractale-orch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sfx = dir.join("blip.orsfx");
        std::fs::write(&sfx, serde_json::to_vec(&orchestre_core::sfx::Sound::default()).unwrap()).unwrap();
        let r = Orchestre.render(&FileInfo::new(sfx.clone(), 10, 0), 128);
        // The default sound is either audible (a waveform) or silent (an
        // error, so the icon shows); it must not hang or panic.
        if let Ok(r) = r {
            assert_eq!(r.still().width(), 128);
        }
        std::fs::write(&sfx, b"{ not json").unwrap();
        assert!(Orchestre.render(&FileInfo::new(sfx.clone(), 10, 0), 128).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
