//! Previews that play while the pointer rests on a tile: videos (frames
//! and sound from `ffmpeg`, at real speed, looping), audio files (decoded with
//! symphonia), and Orchestre sound effects and songs (rendered live by
//! Orchestre's own synth). Sound goes to the default output device through
//! cpal. One preview plays at a time; dropping it stops everything at once.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    /// An Orchestre sound effect (`.orsfx`).
    Sound,
    /// An Orchestre song (`.orch`).
    Song,
}

impl Kind {
    pub fn of(path: &Path) -> Option<Kind> {
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        match ext.as_str() {
            "orsfx" => Some(Kind::Sound),
            "orch" => Some(Kind::Song),
            e if super::thumbs::previewers::video::EXTENSIONS.contains(&e) => Some(Kind::Video),
            "mp3" | "flac" | "ogg" | "oga" | "wav" | "aif" | "aiff" | "aifc" | "m4a" | "aac" | "alac" | "caf" | "mka" => Some(Kind::Audio),
            _ => None,
        }
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One preview playing.
pub struct Playback {
    pub path: PathBuf,
    pub kind: Kind,
    stop: Arc<AtomicBool>,
    frames: Option<Receiver<Frame>>,
    /// Position in the file, 0..1, as f32 bits (when known).
    progress: Arc<AtomicU32>,
    /// The ffmpeg process groups (picture, sound), to kill on stop.
    group: Arc<Mutex<Option<u32>>>,
    audio_group: Arc<Mutex<Option<u32>>>,
}

impl Playback {
    /// Start playing `path`; frames for videos are at most `px` wide/tall.
    pub fn start(path: PathBuf, kind: Kind, px: u32, wake: Arc<dyn Fn() + Send + Sync>) -> Playback {
        let stop = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let group = Arc::new(Mutex::new(None));
        let audio_group = Arc::new(Mutex::new(None));
        let mut frames = None;
        if kind == Kind::Video {
            let (tx, rx) = sync_channel(2);
            frames = Some(rx);
            let (p, s, g) = (path.clone(), stop.clone(), group.clone());
            if let Err(e) = std::thread::Builder::new().name("preview-video".into()).spawn(move || video(&p, px, &s, &g, &tx, &*wake)) {
                eprintln!("fractale: preview thread: {e}");
            }
        }
        // Everything has sound (a video's own track, if it has one).
        let (p, s, g, pr) = (path.clone(), stop.clone(), audio_group.clone(), progress.clone());
        let spawned = std::thread::Builder::new().name("preview-sound".into()).spawn(move || {
            if let Err(e) = sound(&p, kind, &s, &pr, &g) {
                eprintln!("fractale: can't play {}: {e}", p.display());
            }
        });
        if let Err(e) = spawned {
            eprintln!("fractale: preview thread: {e}");
        }
        Playback { path, kind, stop, frames, progress, group, audio_group }
    }

    /// The newest video frame since the last call.
    pub fn frame(&self) -> Option<Frame> {
        self.frames.as_ref()?.try_iter().last()
    }

    pub fn progress(&self) -> Option<f32> {
        let p = f32::from_bits(self.progress.load(Ordering::Relaxed));
        (!p.is_nan()).then_some(p)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for g in [&self.group, &self.audio_group] {
            if let Some(pid) = g.lock().unwrap().take() {
                // SAFETY: plain syscall; the group is ffmpeg's own, and its
                // reader thread reaps it.
                unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            }
        }
    }
}

/// Where a video preview starts: past the opening (often black or a logo).
fn video_start(duration: f64) -> f64 {
    if duration > 4.0 {
        duration * 0.1
    } else {
        0.0
    }
}

/// Frames from ffmpeg, from 10% in, at the video's own pace, looping.
fn video(path: &Path, px: u32, stop: &AtomicBool, group: &Mutex<Option<u32>>, tx: &SyncSender<Frame>, wake: &(dyn Fn() + Send + Sync)) {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Some((duration, w, h)) = super::thumbs::previewers::video::probe(path) else { return };
    let (fw, fh) = super::thumbs::fit(w.max(1), h.max(1), px.max(16));
    let mut start = video_start(duration);
    // Even sizes: some pixel formats need them.
    let (fw, fh) = ((fw & !1).max(2), (fh & !1).max(2));
    while !stop.load(Ordering::Relaxed) {
        let child = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{start:.2}"), "-re", "-i"])
            .arg(path)
            .args(["-an", "-vf", &format!("scale={fw}:{fh},fps=24"), "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        let Ok(mut child) = child else { return };
        *group.lock().unwrap() = Some(child.id());
        let mut out = child.stdout.take().unwrap();
        let mut buf = vec![0u8; (fw * fh * 4) as usize];
        let mut any = false;
        while !stop.load(Ordering::Relaxed) && out.read_exact(&mut buf).is_ok() {
            any = true;
            // A slow UI drops frames rather than queueing them.
            let _ = tx.try_send(Frame { width: fw, height: fh, rgba: buf.clone() });
            wake();
        }
        let _ = child.kill();
        let _ = child.wait();
        group.lock().unwrap().take();
        if !any {
            return;
        }
        // The end: again from the start.
        start = 0.0;
    }
}

/// Samples for the device, interleaved stereo at its rate; false when done.
type Source = Box<dyn FnMut(&mut Vec<f32>) -> bool + Send>;

fn sound(path: &Path, kind: Kind, stop: &Arc<AtomicBool>, progress: &Arc<AtomicU32>, group: &Arc<Mutex<Option<u32>>>) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device().ok_or("no audio output")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let rate = config.sample_rate();
    let mut source: Source = match kind {
        Kind::Audio => audio_file(path, rate, progress.clone())?,
        Kind::Sound => orchestre_sound(path, rate)?,
        Kind::Song => orchestre_song(path, rate, progress.clone())?,
        Kind::Video => video_sound(path, rate, group.clone(), stop.clone())?,
    };
    // Filled here, drained by the device callback (about half a second
    // ahead, so a busy machine doesn't stutter).
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let done = Arc::new(AtomicBool::new(false));
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => out_stream::<f32>(&device, &config.config(), ring.clone(), stop.clone()),
        cpal::SampleFormat::I16 => out_stream::<i16>(&device, &config.config(), ring.clone(), stop.clone()),
        cpal::SampleFormat::I32 => out_stream::<i32>(&device, &config.config(), ring.clone(), stop.clone()),
        cpal::SampleFormat::U16 => out_stream::<u16>(&device, &config.config(), ring.clone(), stop.clone()),
        f => return Err(format!("unsupported sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let ahead = rate as usize; // half a second of stereo
    let mut chunk = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        if ring.lock().unwrap().len() >= ahead {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        chunk.clear();
        if !source(&mut chunk) {
            done.store(true, Ordering::Relaxed);
            break;
        }
        ring.lock().unwrap().extend(chunk.iter().copied());
    }
    // Let what's queued play out, unless stopped.
    while !stop.load(Ordering::Relaxed) && !ring.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(stream);
    Ok(())
}

/// The device stream: stereo from the ring into however many channels it
/// has, with a short fade in (no click) and silence when empty or stopped.
fn out_stream<T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static>(device: &cpal::Device, config: &cpal::StreamConfig, ring: Arc<Mutex<VecDeque<f32>>>, stop: Arc<AtomicBool>) -> Result<cpal::Stream, String> {
    use cpal::traits::DeviceTrait;
    let channels = config.channels as usize;
    let fade_len = config.sample_rate as f32 * 0.03;
    let mut played = 0f32;
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                let mut ring = ring.lock().unwrap();
                let stopped = stop.load(Ordering::Relaxed);
                for frame in data.chunks_mut(channels) {
                    let (l, r) = if stopped { (0.0, 0.0) } else { (ring.pop_front().unwrap_or(0.0), ring.pop_front().unwrap_or(0.0)) };
                    let gain = 0.8 * (played / fade_len).min(1.0);
                    played += 1.0;
                    for (i, s) in frame.iter_mut().enumerate() {
                        let v = if i % 2 == 0 { l } else { r };
                        *s = T::from_sample(v * gain);
                    }
                }
            },
            |e| eprintln!("fractale: audio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// A video's sound track, decoded by ffmpeg straight to the device's rate,
/// from where the picture starts, looping with it. A video without sound
/// ends at once.
fn video_sound(path: &Path, rate: u32, group: Arc<Mutex<Option<u32>>>, stop: Arc<AtomicBool>) -> Result<Source, String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let (duration, _, _) = super::thumbs::previewers::video::probe(path).ok_or("not a video ffprobe can read")?;
    let path = path.to_path_buf();
    let spawn = move |start: f64| {
        let child = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{start:.2}"), "-i"])
            .arg(&path)
            .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", &rate.to_string(), "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()?;
        Some(child)
    };
    let mut child = spawn(video_start(duration)).ok_or("can't run ffmpeg")?;
    *group.lock().unwrap() = Some(child.id());
    let mut any = false;
    let mut buf = vec![0u8; 4096 * 4];
    // A read can end mid-sample: the bytes wait for the next one.
    let mut carry: Vec<u8> = Vec::new();
    Ok(Box::new(move |out: &mut Vec<f32>| loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let n = child.stdout.as_mut().map_or(0, |o| o.read(&mut buf).unwrap_or(0));
        if n > 0 {
            any = true;
            carry.extend_from_slice(&buf[..n]);
            let whole = carry.len() - carry.len() % 4;
            out.extend(carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            carry.drain(..whole);
            return true;
        }
        let _ = child.kill();
        let _ = child.wait();
        if !any {
            // No sound track.
            return false;
        }
        // The end: again from the start, like the picture.
        carry.clear();
        match spawn(0.0) {
            Some(c) => {
                *group.lock().unwrap() = Some(c.id());
                child = c;
                any = false;
            }
            None => return false,
        }
    }))
}

/// An audio file decoded as it plays, resampled to the device's rate.
fn audio_file(path: &Path, rate: u32, progress: Arc<AtomicU32>) -> Result<Source, String> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hint = Hint::new();
    if let Some(e) = path.extension() {
        hint.with_extension(&e.to_string_lossy());
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, MediaSourceStream::new(Box::new(file), Default::default()), FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let id = track.id;
    let total = track.num_frames;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("not audio")?.clone();
    let src_rate = params.sample_rate.unwrap_or(rate);
    let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut buf: Vec<f32> = Vec::new();
    let mut resample = Resampler::new(src_rate, rate);
    let mut frames_done = 0u64;
    Ok(Box::new(move |out: &mut Vec<f32>| loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            _ => return false,
        };
        if packet.track_id != id {
            continue;
        }
        let Ok(b) = decoder.decode(&packet) else { continue };
        let channels = (b.samples_interleaved() / b.frames().max(1)).max(1);
        buf.resize(b.samples_interleaved(), 0.0);
        b.copy_to_slice_interleaved(&mut buf);
        frames_done += b.frames() as u64;
        if let Some(t) = total.filter(|&t| t > 0) {
            progress.store(((frames_done as f32 / t as f32).min(1.0)).to_bits(), Ordering::Relaxed);
        }
        let stereo = buf.chunks(channels).flat_map(|f| [f[0], *f.get(1).unwrap_or(&f[0])]);
        resample.push(stereo, out);
        return true;
    }))
}

fn orchestre_sound(path: &Path, rate: u32) -> Result<Source, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let sound = orchestre_core::sfx::file::parse_sound_bytes(&bytes)?;
    let mut player = orchestre_dsp::SoundPlayer::new(&sound, &orchestre_core::sfx::PlayOpts::default(), rate as f32);
    let release_at = sound.loop_preview().map(|s| (s.min(10.0) * rate as f32) as usize);
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    let mut done = 0usize;
    Ok(Box::new(move |out: &mut Vec<f32>| {
        // At most 10 s, like the waveform.
        if player.is_done() || done > 10 * rate as usize {
            return false;
        }
        if release_at.is_some_and(|at| done >= at) {
            player.release();
        }
        player.process(&mut l, &mut r);
        done += 512;
        out.extend(l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]));
        true
    }))
}

fn orchestre_song(path: &Path, rate: u32, progress: Arc<AtomicU32>) -> Result<Source, String> {
    use orchestre_dsp::{Cmd, Engine, Song};
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let project = orchestre_core::file::parse_project_bytes(&bytes)?;
    let song = Song::from_project(&project);
    let seconds = orchestre_core::ticks_to_seconds(project.length_ticks() as f64, project.bpm as f64).max(0.1);
    let mut engine = Engine::new(rate as f32);
    engine.handle(Cmd::SetSong(Box::new(song)));
    engine.handle(Cmd::SetLoop(None));
    engine.handle(Cmd::Seek(0));
    engine.handle(Cmd::Play);
    let mut buf = vec![0f32; 1024];
    let mut played = 0f64;
    Ok(Box::new(move |out: &mut Vec<f32>| {
        if played / rate as f64 > seconds + 2.0 {
            return false;
        }
        engine.process_interleaved(&mut buf, 2);
        let _ = engine.drain_events().count();
        played += (buf.len() / 2) as f64;
        progress.store(((played / rate as f64 / seconds).min(1.0) as f32).to_bits(), Ordering::Relaxed);
        out.extend_from_slice(&buf);
        true
    }))
}

/// Linear resampling of interleaved stereo: plenty for a preview.
struct Resampler {
    step: f64,
    pos: f64,
    last: [f32; 2],
}

impl Resampler {
    fn new(from: u32, to: u32) -> Resampler {
        Resampler { step: from as f64 / to.max(1) as f64, pos: 0.0, last: [0.0; 2] }
    }

    fn push(&mut self, stereo: impl Iterator<Item = f32>, out: &mut Vec<f32>) {
        let input: Vec<f32> = stereo.collect();
        let frames = input.len() / 2;
        let at = |i: isize| -> [f32; 2] {
            if i < 0 {
                self.last
            } else {
                [input[i as usize * 2], input[i as usize * 2 + 1]]
            }
        };
        // Positions are relative to this chunk, -1 being the last frame of
        // the previous one.
        while self.pos < frames as f64 - 1.0 {
            let i = self.pos.floor() as isize;
            let t = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a[0] + (b[0] - a[0]) * t);
            out.push(a[1] + (b[1] - a[1]) * t);
            self.pos += self.step;
        }
        if frames > 0 {
            self.last = at(frames as isize - 1);
            self.pos -= frames as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_length_and_shape() {
        let mut r = Resampler::new(22050, 44100);
        let mut out = Vec::new();
        for _ in 0..10 {
            let chunk: Vec<f32> = (0..441).flat_map(|i| [i as f32, -(i as f32)]).collect();
            r.push(chunk.into_iter(), &mut out);
        }
        // 10 × 441 frames at double the rate.
        let frames = out.len() / 2;
        assert!((frames as i64 - 8820).abs() <= 2, "{frames}");
        assert!(out.chunks(2).all(|f| (f[0] + f[1]).abs() < 1e-3), "channels kept apart");
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::of(Path::new("a.MP4")), Some(Kind::Video));
        assert_eq!(Kind::of(Path::new("a.flac")), Some(Kind::Audio));
        assert_eq!(Kind::of(Path::new("a.orsfx")), Some(Kind::Sound));
        assert_eq!(Kind::of(Path::new("a.orch")), Some(Kind::Song));
        assert_eq!(Kind::of(Path::new("a.txt")), None);
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    /// A second of silence, then a tone, as a 16-bit mono WAV.
    fn wav(path: &Path, rate: u32) {
        let mut data = Vec::new();
        for i in 0..rate {
            let s = (i as f32 * 0.05).sin() * 0.5;
            data.extend(((s * i16::MAX as f32) as i16).to_le_bytes());
        }
        let mut f = b"RIFF".to_vec();
        f.extend((36 + data.len() as u32).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(rate.to_le_bytes());
        f.extend((rate * 2).to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        std::fs::write(path, f).unwrap();
    }

    fn drain(mut s: Source) -> Vec<f32> {
        let mut all = Vec::new();
        let mut chunk = Vec::new();
        while s(&mut chunk) {
            all.append(&mut chunk);
            assert!(all.len() < 48_000 * 2 * 60, "never ends");
        }
        all
    }

    #[test]
    fn sources_make_sound_at_the_device_rate() {
        let dir = std::env::temp_dir().join(format!("fractale-play-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tone.wav");
        wav(&p, 22050);
        let progress = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let out = drain(audio_file(&p, 48000, progress.clone()).unwrap());
        // One second, resampled to 48 kHz stereo.
        assert!((out.len() as i64 - 96_000).abs() < 600, "{}", out.len());
        assert!(out.iter().any(|s| s.abs() > 0.3));
        assert_eq!(f32::from_bits(progress.load(Ordering::Relaxed)), 1.0);

        let sfx = dir.join("blip.orsfx");
        std::fs::write(&sfx, orchestre_core::sfx::file::sound_to_json(&orchestre_core::sfx::Sound::default()).unwrap()).unwrap();
        let _ = drain(orchestre_sound(&sfx, 48000).unwrap());
        let demo = Path::new(concat!(env!("HOME"), "/Projects/orchestre/crates/bevy/assets/music/demo.orch"));
        if demo.exists() {
            let pr = Arc::new(AtomicU32::new(0));
            let mut s = orchestre_song(demo, 48000, pr.clone()).unwrap();
            let mut chunk = Vec::new();
            for _ in 0..200 {
                assert!(s(&mut chunk));
            }
            assert!(chunk.iter().any(|v| v.abs() > 1e-3), "the song plays");
            assert!(f32::from_bits(pr.load(Ordering::Relaxed)) > 0.0);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn video_frames_come_and_stop() {
        if !super::super::thumbs::previewers::video::available() {
            eprintln!("no ffmpeg: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fractale-playv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=25", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(ok.success());
        let play = Playback::start(clip.clone(), Kind::Video, 128, Arc::new(|| {}));
        let start = std::time::Instant::now();
        let frame = loop {
            if let Some(f) = play.frame() {
                break f;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "no frame");
            std::thread::sleep(Duration::from_millis(10));
        };
        eprintln!("first frame after {:?}", start.elapsed());
        assert_eq!((frame.width, frame.height), (128, 96));
        assert_eq!(frame.rgba.len(), 128 * 96 * 4);
        let pid = *play.group.lock().unwrap();
        drop(play);
        std::thread::sleep(Duration::from_millis(300));
        if let Some(pid) = pid {
            // SAFETY: signal 0 only checks whether the process exists.
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
            assert!(!alive, "ffmpeg stopped with the preview");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Through the real output device (opt-in: `cargo test -- --ignored`).
    /// Plays a second of silence, so it's quiet on a desk.
    #[test]
    #[ignore]
    fn plays_through_the_device() {
        let dir = std::env::temp_dir().join(format!("fractale-dev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("silence.wav");
        let rate = 22050u32;
        let data = vec![0u8; rate as usize * 2];
        let mut f = b"RIFF".to_vec();
        f.extend((36 + data.len() as u32).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(rate.to_le_bytes());
        f.extend((rate * 2).to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        std::fs::write(&p, f).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let start = std::time::Instant::now();
        sound(&p, Kind::Audio, &stop, &progress, &Arc::new(Mutex::new(None))).unwrap();
        eprintln!("played a 1 s file in {:?}", start.elapsed());
        assert_eq!(f32::from_bits(progress.load(Ordering::Relaxed)), 1.0);
        assert!(start.elapsed() >= Duration::from_millis(800), "it played in real time");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_videos_sound_track_plays_and_a_silent_video_ends() {
        if !super::super::thumbs::previewers::video::available() {
            eprintln!("no ffmpeg: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fractale-vsound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (with, without) = (dir.join("with.mp4"), dir.join("without.mp4"));
        let make = |out: &Path, sound: bool| {
            let mut c = std::process::Command::new("ffmpeg");
            c.args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x120:rate=25"]);
            if sound {
                c.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:a", "aac"]);
            }
            c.args(["-pix_fmt", "yuv420p", "-shortest"]).arg(out);
            assert!(c.status().unwrap().success());
        };
        make(&with, true);
        make(&without, false);
        let stop = Arc::new(AtomicBool::new(false));
        let g = Arc::new(Mutex::new(None));
        let mut s = video_sound(&with, 48000, g.clone(), stop.clone()).unwrap();
        let mut got = Vec::new();
        let mut chunk = Vec::new();
        while got.len() < 48000 * 2 && s(&mut chunk) {
            got.append(&mut chunk);
        }
        assert!(got.len() >= 48000 * 2, "a second of stereo: {}", got.len());
        // lavfi's sine is at 1/8 amplitude.
        let peak = got.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05, "the tone is there: peak {peak}");
        stop.store(true, Ordering::Relaxed);
        if let Some(pid) = g.lock().unwrap().take() {
            // SAFETY: plain syscall on ffmpeg's own group.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
        let mut silent = video_sound(&without, 48000, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false))).unwrap();
        assert!(!silent(&mut chunk), "no sound track: ends at once");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
