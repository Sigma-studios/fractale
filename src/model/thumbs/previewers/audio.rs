//! Audio files: a waveform (peak level over time), decoded with symphonia.
//!
//! Short files are decoded whole. Long ones (where the length is known)
//! are sampled: a seek and a few packets per column, so an hour-long
//! recording costs about as much as a song.

use super::super::{FileInfo, Previewer, Rendered};
use std::time::{Duration, Instant};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

const EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "oga", "wav", "aif", "aiff", "aifc", "m4a", "aac", "alac", "caf", "mka", "mp2", "mp1"];
/// Files longer than this are sampled rather than decoded whole.
const SAMPLE_ABOVE: Duration = Duration::from_secs(8 * 60);
const SAMPLED_COLUMNS: usize = 160;
const BUDGET: Duration = Duration::from_secs(8);
/// Waveform colour (the theme's violet).
pub const COLOR: [u8; 3] = [0x90, 0x85, 0xe9];

pub struct Waveform;

impl Previewer for Waveform {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        EXTENSIONS.contains(&f.ext.as_str())
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        let peaks = peaks(f)?;
        Ok(Rendered::Still(draw(&peaks, max, COLOR)))
    }
}

/// Peak levels (0..1) over the file, as many as were measured.
fn peaks(f: &FileInfo) -> Result<Vec<f32>, String> {
    let start = Instant::now();
    let file = std::fs::File::open(&f.path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(&f.ext);
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let (id, start_ts) = (track.id, track.start_ts.get());
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("not audio")?.clone();
    let length = match (track.num_frames, params.sample_rate) {
        (Some(n), Some(sr)) if sr > 0 => Some((n, Duration::from_secs_f64(n as f64 / sr as f64))),
        _ => None,
    };
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut buf: Vec<f32> = Vec::new();
    // Peak of one decoded packet.
    let mut peak_of = |b: &symphonia::core::audio::GenericAudioBufferRef| {
        buf.resize(b.samples_interleaved(), 0.0);
        b.copy_to_slice_interleaved(&mut buf);
        buf.iter().fold(0f32, |m, s| m.max(s.abs()))
    };

    if let Some((frames, len)) = length.filter(|(_, len)| *len > SAMPLE_ABOVE) {
        let mut out = Vec::with_capacity(SAMPLED_COLUMNS);
        for k in 0..SAMPLED_COLUMNS {
            if start.elapsed() > BUDGET {
                return Err(format!("sampling {len:?} took too long"));
            }
            let ts = start_ts + (frames as u128 * k as u128 / SAMPLED_COLUMNS as u128) as i64;
            if format.seek(SeekMode::Coarse, SeekTo::Timestamp { ts: Timestamp::new(ts), track_id: id }).is_err() {
                break;
            }
            decoder.reset();
            let mut peak = 0f32;
            let mut got = 0;
            while got < 3 {
                let Ok(Some(packet)) = format.next_packet() else { break };
                if packet.track_id != id {
                    continue;
                }
                if let Ok(b) = decoder.decode(&packet) {
                    peak = peak.max(peak_of(&b));
                }
                got += 1;
            }
            out.push(peak);
        }
        if out.len() == SAMPLED_COLUMNS {
            return Ok(out);
        }
        return Err("couldn't seek".into());
    }

    // Whole file: one peak per packet (a few ms each), resampled when drawn.
    let mut out = Vec::new();
    loop {
        if start.elapsed() > BUDGET {
            return Err("decoding took too long".into());
        }
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(_) if !out.is_empty() => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(b) => out.push(peak_of(&b)),
            Err(symphonia::core::errors::Error::DecodeError(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if out.is_empty() {
        return Err("no audio".into());
    }
    Ok(out)
}

/// A mirrored waveform, `max` wide and half as tall, on transparent.
/// Levels are normalised to the loudest so quiet files still show shape.
pub fn draw(levels: &[f32], max: u32, rgb: [u8; 3]) -> image::RgbaImage {
    let (w, h) = (max.max(16), (max / 2).max(8));
    let mut img = image::RgbaImage::new(w, h);
    let loudest = levels.iter().fold(0f32, |m, &v| m.max(v)).max(1e-6);
    let mid = h as f32 / 2.0;
    for x in 0..w {
        // The loudest level among the ones this column covers.
        let a = x as usize * levels.len() / w as usize;
        let b = ((x as usize + 1) * levels.len() / w as usize).max(a + 1).min(levels.len());
        let v = levels.get(a..b).map_or(0.0, |s| s.iter().fold(0f32, |m, &v| m.max(v))) / loudest;
        let half = (v * (mid - 1.0)).max(0.5);
        for y in 0..h {
            let d = (y as f32 + 0.5 - mid).abs();
            // Anti-aliased top and bottom edges.
            let cover = (half - d + 0.5).clamp(0.0, 1.0);
            if cover > 0.0 {
                img.put_pixel(x, y, image::Rgba([rgb[0], rgb[1], rgb[2], (cover * 255.0) as u8]));
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 16-bit WAV: a second of silence then a second of a loud tone.
    fn wav(path: &std::path::Path) {
        let sr = 8000u32;
        let mut data = Vec::new();
        for i in 0..2 * sr {
            let s = if i < sr { 0.0 } else { (i as f32 * 0.3).sin() * 0.8 };
            data.extend(((s * i16::MAX as f32) as i16).to_le_bytes());
        }
        let mut f = Vec::new();
        f.extend(b"RIFF");
        f.extend((36 + data.len() as u32).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(sr.to_le_bytes());
        f.extend((sr * 2).to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        std::fs::write(path, f).unwrap();
    }

    #[test]
    fn waveform_follows_the_sound() {
        let dir = std::env::temp_dir().join(format!("fractale-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tone.wav");
        wav(&p);
        let md = std::fs::metadata(&p).unwrap();
        let info = FileInfo::new(p.clone(), md.len(), 0);
        let img = Waveform.render(&info, 128).unwrap();
        let img = img.still();
        assert_eq!((img.width(), img.height()), (128, 64));
        let column = |x: u32| (0..img.height()).filter(|&y| img.get_pixel(x, y)[3] > 128).count();
        assert!(column(10) <= 2, "silence is a thin line: {}", column(10));
        assert!(column(100) > 40, "the tone fills the height: {}", column(100));
        std::fs::write(&p, b"RIFF\0\0\0\0WAVEjunk").unwrap();
        assert!(Waveform.render(&FileInfo::new(p.clone(), 16, 0), 128).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
