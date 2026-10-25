//! Resting the pointer on a video plays it in its tile after 200 ms;
//! moving away stops it.

mod common;

use common::*;
use egui_kittest::kittest::Queryable;
use std::time::{Duration, Instant};

#[test]
fn hovering_a_video_plays_it_and_leaving_stops_it() {
    let s = Scratch::new("media");
    let clip = s.0.join("clip.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=25", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status()
        .is_ok_and(|s| s.success());
    if !made || !fractale::model::thumbs::previewers::video::available() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    s.file("notes.txt", 10);
    let mut h = window(&s.0);
    listed(&mut h);
    let tile = h.get_by_label("clip.mp4").rect().center();
    h.event(egui::Event::PointerMoved(tile));
    let start = Instant::now();
    // Not before the delay.
    h.run_steps(3);
    assert!(h.state().media.playing.is_none());
    let took = until(&mut h, Duration::from_secs(5), "playback", |w| w.media.is_playing(&clip));
    eprintln!("started after {:?}", start.elapsed());
    assert!(start.elapsed() >= Duration::from_millis(190), "waited the delay ({took:?})");
    // Frames reach the tile.
    let frame_seen = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            h.step();
            if h.state().media.has_frame() {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    assert!(frame_seen, "a frame was shown");
    // Away: stops.
    let away = h.get_by_label("notes.txt").rect().center();
    h.event(egui::Event::PointerMoved(away));
    h.run_steps(2);
    assert!(h.state().media.playing.is_none());
}
