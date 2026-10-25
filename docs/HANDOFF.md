# Hand-off

What Sigma needs to package Fractale, what was measured, what's open, and
what to check on the real machine (NixOS, KDE Plasma 6 on Wayland,
2560×1440 at 144 Hz). Everything was built on macOS and checked headless
in a Lima VM (Ubuntu 24.04, kernel 6.8, aarch64); no Linux GUI has been
looked at.

## Measured so far

| What | Result | Target |
|---|---|---|
| Worst UI frame while opening and scrolling a 10,000-file folder (release, macOS, CPU side) | 1.9 ms | < 16 ms |
| Same in the real app (`FRACTALE_FRAME_STATS=1`, release, macOS) | 2.6 ms worst, first second included | |
| A change in the open folder (another process) visible after | 39 ms (macOS, FSEvents), 52 ms (VM, inotify) | < 300 ms |
| Fallback sizes vs `du -s` (macOS `-k`, Linux `--block-size=1`) | equal, folder and every subfolder | equal |
| Switching folders while the fallback is measuring a big one | 7.5 ms until the new folder is being measured | |
| Expanding a row (fallback, already measured) | 12 ms | |
| répertoire sizes vs Fractale's own (fixtures, macOS, daemon in scan-only mode) | equal, total and every subfolder | equal |
| A 1 GB write into a subfolder, live daemon (VM, root, fanotify) | row updated 3 ms after the write finished | < 2 s |
| Daemon stopped → own measuring | 2 ms, numbers stay on screen | no gap |
| Daemon started again → back on it | within the retry period (60 s; 0.3 s in tests) | < 1 min |
| Search results, Fractale vs `repertoire search` | identical; without the daemon the walk finds the same set | equal |
| 5,000 photos (1600×1200 JPEGs, some PNGs), window 2560×1440 at 1×, cold cache: first screen of previews (144) | 287 ms (release, macOS) | < 1 s |
| Same, worst UI frame scrolling at 144 Hz, 40 / 100 / 300 px a frame (CPU side) | 1.6 / 1.8 / 5.3 ms | < 6.9 ms |
| Same, tiles still on their icon while scrolling | none at 40 px a frame; at 100+ px the screen fills 0.44 s after stopping | |
| Same, memory after 4 s of scrolling | 137–188 MB of textures (cap 256 MB), peak RSS 221 MB | |
| One preview, serially: 1600×1200 JPEG / song / sound effect / Pixelle sprite | 8 ms / 0.8 ms / 5 ms / 0.6 ms | |
| A video through ffmpegthumbnailer (VM) | 48 ms | |
| XCF previews vs GIMP 2.10's own flattening (13 fixtures) | mean difference ≤ 0.03 of 255 | |

## Known limits

- **Fallback sizes aren't live below the open folder.** Without répertoire,
  Fractale watches only the open folder (one `notify` watch), so a file
  written into a *subfolder* doesn't update that subfolder's bar until F5,
  or until the folder is visited again after 10 minutes. Changes directly
  in the open folder are picked up by themselves. With répertoire running,
  everything is live.
- With the fallback, a hard link whose links are in two subfolders is
  charged to whichever was measured first (the folder total is right
  either way). répertoire charges the first link it saw.

- **Sizes come from répertoire per folder.** A folder the daemon doesn't
  index (tmpfs, a stick under 2 GiB, `/proc`) is measured by Fractale.
- répertoire's state for the open folder's filesystem is the badge at the
  bottom left: *live*, *reconciling N%*, *scanning N%*, *stale N%*,
  *static* (daemon not watching), *own scan* (no daemon).

## Previews

Previewers are tried in order; the first that succeeds wins, its result
goes to the shared freedesktop cache (`~/.cache/thumbnails`, so Dolphin
and Fractale share work) and failures to `fail/fractale-<version>/`.

| Files | How |
|---|---|
| PNG, JPEG, WebP, BMP, TIFF, QOI, TGA, ICO, HDR, PNM | in-process (`image`), JPEGs decoded at 1/2–1/8 scale, EXIF orientation applied |
| GIF | animated in the grid (first 150 frames) |
| XCF | own reader: every version and precision, RLE/zlib, visible layers, opacity, masks; blend modes drawn as normal |
| Pixelle `.pxl` | own parser: sprites (frame 0, exported layers, project palette), terrains (tile sheet), `project.pxl` (palette) |
| Orchestre `.orch` / `.orsfx` | piano roll in track colours / waveform rendered by Orchestre's synth (git dependency, pinned) |
| Prisme `.prisme` | the `PREV` chunk if the file has one, otherwise the icon (see below) |
| Audio (MP3, FLAC, Ogg Vorbis, WAV, AIFF, M4A/AAC/ALAC, CAF) | waveform (symphonia); files over 8 minutes are sampled |
| Everything else (Linux) | every installed `*.thumbnailer` |

For Nix: video, PDF and font previews come from the thumbnailers on
`XDG_DATA_DIRS`, e.g. `ffmpegthumbnailer` (videos), `evince` or `papers`
(PDFs), `gnome-epub-thumbnailer`. KDE's own thumbnailers
(`kdegraphics-thumbnailers`, `ffmpegthumbs`) are KIO plugins, not
`.thumbnailer` files, and aren't used. The file's MIME
type comes from shared-mime-info's `globs2` (`shared-mime-info` package),
by name only.

- **Prisme files have no preview yet.** Prisme can embed one
  (`prisme_io::container::save_with_preview`) but the app saves with
  `None` (`crates/prisme-ui/src/offload.rs:87` and `:172`). Passing the
  flattened canvas there (a small change in Prisme) makes them show up
  here with no change to Fractale. Not done: it's Prisme's code.
- Failures are remembered until the file changes; installing a
  thumbnailer later doesn't retry files that a previewer had already
  failed on (types nothing handled aren't recorded, so they do get one).
- macOS has no `.thumbnailer`s: videos and PDFs show icons there.

## To check on the target machine

1. Frame times at 144 Hz with a big folder (`FRACTALE_FRAME_STATS=1
   fractale ~/some/10k-file/folder` prints the worst frame each second).
2. With répertoire running as the system service: the badge says *live*,
   bars move while a big download or build runs in a subfolder, and
   `systemctl stop repertoire` falls back to *own scan* without the bars
   blinking.
3. The fallback's priority: measuring `~` (520k files) without répertoire
   must not make the desktop or a game stutter (nice 19, idle I/O class).
4. Previews: scroll a big photo folder at 144 Hz (texture uploads are
   capped at 2 Mpx a frame; the GPU side wasn't measured here), and check
   that `ffmpegthumbnailer` installed through Nix is found (it's looked
   up on `XDG_DATA_DIRS`).
