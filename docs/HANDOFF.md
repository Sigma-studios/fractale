# Hand-off

This covers what Sigma needs to package Fractale, what was measured, what's
open, and what to check on the real machine (NixOS, KDE Plasma 6 on Wayland,
2560×1440 at 144 Hz).

It was built on macOS and checked in a Lima VM (Ubuntu 24.04, kernel 6.8,
aarch64): headless, plus a headless KWin 5.27 and 6.8 for the Wayland
clipboard and drag and drop. Nobody has looked at it on a Linux desktop
yet.

## Packaging (Nix)

- **Build:** `cargo build --release`. The only system library needed at
  build time is ALSA (`alsa-lib`, found with `pkg-config`), for sound in
  hover previews (cpal). Two git dependencies are pinned by revision and
  need `cargoLock.outputHashes`:
  - répertoire `fd764be` (`repertoire-core`, `repertoire-proto`; the
    daemon crate itself is dev-only);
  - Orchestre `9969a78` (`orchestre-core`, `orchestre-dsp`).
- **Runtime libraries:** everything graphical is `dlopen`ed (`ldd` shows
  only libc), so the wrapper must put these on `LD_LIBRARY_PATH`, the
  same set as Sentinelle's:
  - `libGL` (libglvnd, for EGL/GLX)
  - `wayland`
  - `libxkbcommon`
  - for X11 sessions: `xorg.libX11`, `libXcursor`, `libXrandr`, `libXi`
- **Runtime data:** shared-mime-info (in every NixOS profile) gives file
  types. Desktop entries and `mimeapps.list` choose apps, exactly as
  Dolphin does. Thumbnailers on `XDG_DATA_DIRS` add previews (see
  *Previews*).
- **`ffmpeg` on the wrapper's `PATH`:** with it, videos get a still (when
  no thumbnailer took them) and play on hover. Without it they show icons.
- **Sound:** through ALSA, which NixOS routes to PipeWire
  (`pipewire.alsa.enable`).
- **Optional:** `xdg-utils`. `xdg-open` is the last resort when no app is
  known for a type.
- **Files to install from `dist/`:**
  - `fractale.desktop` into `share/applications`. It sets
    `MimeType=inode/directory`; it is validated, but doesn't make Fractale
    the default (Sigma's choice).
  - `org.freedesktop.FileManager1.service` into `share/dbus-1/services`,
    only if Fractale *is* the file manager, since Dolphin ships the same
    file. Replace its `Exec` path with the store path.
- **Session bus:** a running Fractale owns
  `io.github.Sigma_studios.Fractale`, and queues for
  `org.freedesktop.FileManager1` without taking it from Dolphin.

## Measured

| What | Result | Target |
|---|---|---|
| Worst UI frame, opening and scrolling a 10,000-file folder (release, macOS, CPU side) | 1.9 ms | < 16 ms |
| Same in the real app (`FRACTALE_FRAME_STATS=1`, release, macOS) | 2.6 ms worst, first second included | |
| Change in the open folder by another process, until visible | 39 ms (macOS, FSEvents), 52 ms (VM, inotify) | < 300 ms |
| Fallback sizes vs `du -s` | equal, folder and every subfolder | equal |
| Switching folders while the fallback measures a big one | 7.5 ms until the new folder is measured | |
| Expanding a row (fallback, already measured) | 12 ms | |
| répertoire sizes vs Fractale's own | equal, total and every subfolder | equal |
| 1 GB write into a subfolder, live daemon (VM, root, fanotify) | row updated 3 ms after the write finished | < 2 s |
| Daemon stopped → own measuring | 2 ms, numbers stay on screen | no gap |
| Daemon started again → back on it | within the retry period (60 s; 0.3 s in tests) | < 1 min |
| Search results, Fractale vs `repertoire search` | identical; without the daemon the walk finds the same set | equal |
| 5,000 photos, 2560×1440 at 1×, cold cache: first screen of previews (144) | 284–289 ms (macOS, 8 workers); 325–370 ms (VM, 6 vCPUs, images over virtiofs) | < 1 s |
| Same, worst UI frame scrolling at 144 Hz, 40 / 100 / 300 px a frame (CPU side) | macOS 2.5 / 2.7 / 2.2 ms; VM 2.6 / – / 2.5 ms | < 6.9 ms |
| Same, tiles still on their icon while scrolling at 40 px a frame (5,760 px/s) | macOS none; VM in most frames (decodes ~2× slower) | |
| Same, after a fast flick (100 px a frame) | screen full 0.49 s after stopping (macOS) | |
| Same, memory after 4 s of scrolling | ≤ 206 MB of textures (cap 256 MB); peak RSS 197 MB (macOS), 146 MB (VM) | |
| One preview, serially: 1600×1200 JPEG / song / sound effect / Pixelle sprite | 8 ms / 0.8 ms / 5 ms / 0.6 ms | |
| Video through ffmpegthumbnailer (VM) | 48 ms | |
| XCF previews vs GIMP 2.10's own flattening (13 fixtures) | mean difference ≤ 0.03 of 255 | |
| Move of a test tree to another filesystem and back (loopback ext4, VM) | 70 ms; sources removed only once copied and synced | |
| Cancel during a 512 MB copy | no partial file left | none |
| MIME types vs KDE (`kmimetypefinder`), 998 real files (VM) | identical | |
| Loading the MIME and app databases | ~2 ms warm, ≤ 16 ms cold (VM) | |
| Second launch handing over to the running window | 10 ms, then exits | |
| Clipboard and drag and drop with other apps, real binary in headless KWin 6.8.1 (`scripts/e2e/run.sh`) | all 7 checks pass: copy/cut to `wl-paste` (with KDE's cut marker), paste from `wl-copy` and from a cut, drops onto the file area and onto a folder row, drag out to another app, drag onto a folder row inside, text paste into the search field | |

## Previews

Previewers are tried in order; the first that succeeds wins. Results go to
the shared freedesktop cache (`~/.cache/thumbnails`), so Dolphin and
Fractale share work. Failures go to `fail/fractale-<version>/`.

| Files | How |
|---|---|
| PNG, JPEG, WebP, BMP, TIFF, QOI, TGA, ICO, HDR, PNM | in-process (`image`); JPEGs decoded at 1/2–1/8 scale; EXIF orientation applied |
| GIF | animated in the grid (first 150 frames) |
| XCF | own reader: every version and precision, RLE/zlib, visible layers, opacity, masks; blend modes drawn as normal |
| Pixelle `.pxl` | own parser: sprites (frame 0, exported layers, project palette), terrains (tile sheet), `project.pxl` (palette) |
| Orchestre `.orch` / `.orsfx` | piano roll in track colours / waveform from Orchestre's own synth |
| Prisme `.prisme` | the embedded `PREV` image, otherwise the icon (see below) |
| Audio (MP3, FLAC, Ogg Vorbis, WAV, AIFF, M4A/AAC/ALAC, CAF) | waveform (symphonia); files over 8 minutes are sampled |
| SVG, SVGZ | rendered in-process (resvg) |
| PDF | first page, rendered in-process (hayro, pure Rust); encrypted files get the icon |
| Word, Excel, PowerPoint, LibreOffice | the preview picture the file carries (LibreOffice always writes one; Office only when asked); a Word file without one gets its first lines of text on a page |
| HTML | title and text on a page, read without fetching or running anything |
| Fonts (TTF, OTF, TTC) | "Aa", a sample line and the family name, drawn with the font |
| Everything else (Linux) | every installed `*.thumbnailer`, matched by MIME type (name, then content) |
| Videos nothing else took | one frame from 10% in, with `ffmpeg` |

**Playing on hover:** resting the pointer on a video, an audio file or an
Orchestre file for a second plays it, and moving away stops it.
- Videos play muted inside the tile, from 10% in, looping.
- Audio files play from the start through the default output, with a
  progress bar.
- Orchestre sound effects and songs are rendered live by Orchestre's own
  synth.

Measured on macOS: a video's first frame arrives 115 ms after it starts.
Playback starts 1.0 s after the pointer stops.

**Thumbnailer packages worth installing:**
- `ffmpegthumbnailer` (videos)
- `evince` or `papers` (PDFs)
- `gnome-epub-thumbnailer` (ebooks)
- `gnome-font-viewer` (fonts)

KDE's thumbnailers (`kdegraphics-thumbnailers`, `ffmpegthumbs`) are KIO
plugins, not `.thumbnailer` files, and can't be used.

## Known gaps and limits

- **Prisme files have no preview yet.** Prisme can embed one
  (`prisme_io::container::save_with_preview`), but the app saves with
  `None` (`crates/prisme-ui/src/offload.rs:87` and `:172`). Passing the
  flattened canvas there is a small change in Prisme and needs nothing
  from Fractale. I didn't make it: it's Prisme's code.
- **Fallback sizes aren't live below the open folder.** Without
  répertoire, Fractale watches only the open folder. A file written into
  a subfolder updates that subfolder's bar after F5, or when the folder is
  visited again after 10 minutes. With répertoire everything is live.
- **Hard links under the fallback:** a file with links in two subfolders
  is charged to whichever was measured first. The total is right either
  way.
- **One window,** as asked. A second launch, or another app's "show in
  folder", goes to that window, which asks to be raised. On Wayland KWin
  may flash it in the task bar rather than raise it, since raising needs
  an activation token that Fractale doesn't pass on yet. No tabs.
- **Clipboard and drag and drop with other apps are Wayland only.**
  - Under X11 (and macOS), files are copied and dragged only inside
    Fractale. Text fields use the X11 clipboard through arboard; that path
    hasn't been run under X11.
  - Under Plasma 5, Shift during another app's drop doesn't turn it into
    a move. KWin 5 applies Shift only when the target re-sends its
    actions; KWin 6 applies it directly (checked).
  - Middle-click paste (primary selection) isn't supported.
  - A drop that asks ("ask" action) is treated as a copy.
  - Scales other than 1 and multiple seats weren't tested.
- **Copying details:**
  - Folders merge silently; only files inside them conflict.
  - "Replace" of a folder by a file (or the reverse) removes what was
    there.
  - Special files (pipes, sockets, devices) are reported and skipped.
- **Open with:**
  - Apps whose `Exec` program isn't on `PATH` are hidden, as GLib does,
    so a Fractale started with a minimal `PATH` shows fewer apps.
  - No "set as default" yet.
- **Previews of documents are approximations:** HTML and Word files
  without a preview picture show their text, not their layout.
  woff/woff2 fonts and encrypted PDFs get icons. The first SVG, document
  page or font of a run waits about 1 s while the system fonts load.
- **Hover playback:** videos are muted, and only grid tiles play (not the
  list).
- **Previews:** failures are remembered until the file changes. A
  thumbnailer installed later doesn't retry files a previewer already
  failed on (types nothing handled aren't recorded, so those do get
  one).
- **Network locations** (KIO's `smb:/`, `sftp:/`…) are out of scope.

## Choices made along the way

- **Drag out of Fractale and the system clipboard are in v1** (the plan
  had them optional; drag in/out was asked for). winit 0.30 has neither,
  so Fractale runs its own Wayland data device on winit's connection: a
  worker thread with its own seat, pointer, keyboard and data device
  (`src/model/platform/wayland/`). eframe's clipboard feature is off on
  Linux because of it.
- **zbus 5**, not 6: zbus 6 (October 2026) dropped the blocking API, and
  Fractale has no async runtime.
- **Previews run at nice 10** (lowest best-effort I/O priority on Linux,
  the utility QoS class on macOS), not at the sizing scans' idle priority.
  The background class made previews about ten times slower on macOS
  (1.67 s for the first screen instead of 0.29 s), because it confines
  threads to the efficiency cores.
- **répertoire's protocol is unchanged.**

## To check on the target machine

1. **Frame times** at 144 Hz with a big folder, and with a big photo
   folder (`FRACTALE_FRAME_STATS=1 fractale ~/some/folder` prints the
   worst frame each second). Texture uploads are capped at 2 Mpx a frame,
   but the GPU side wasn't measured here.
2. **répertoire** running as the system service:
   - the badge says *live*;
   - bars move while a big download or build runs in a subfolder;
   - `systemctl stop repertoire` falls back to *own scan* without the
     bars blinking.
3. **The fallback's priority:** measuring `~` without répertoire must not
   make the desktop or a game stutter (nice 19, idle I/O class).
4. **Dolphin interop:**
   - copy in Fractale, paste in Dolphin, and the reverse;
   - cut in Dolphin, paste in Fractale (moves, once);
   - drag files from Dolphin onto a folder row, and from Fractale into
     Dolphin, Firefox (an upload field) and Discord.
5. **Trash:** an item trashed in Fractale shows in Dolphin's `trash:/`
   and restores to where it was. Tested here with gio, which follows the
   same spec.
6. **Show in folder:** "show in folder" from Firefox's downloads lands in
   Fractale with the file selected. This happens only while Fractale
   holds `org.freedesktop.FileManager1`, i.e. no Dolphin running, or the
   service file installed.
7. **Opening files:** Enter opens with the same app Dolphin would use;
   "Open with…" lists the same apps; running a script asks first.
8. **Thumbnailers:** `ffmpegthumbnailer` installed through Nix is found
   (it's looked up on `XDG_DATA_DIRS`), and videos get previews.
