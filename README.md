# Fractale

A file explorer for everyday browsing that also shows what takes the space.
Folders are on the left as rows with size bars, each bar showing the
folder's share of the open folder. Below them, rows for the files directly
in the folder and for hidden entries (while they aren't shown) make the bars
add up to the total. Expanded rows follow the pane's sort. The eye on a row
(or Space) shows that folder's files on the right without opening it. Everything else
is on the right, as a grid of previews or a list. Sizes and search come from
[répertoire](https://github.com/Sigma-studios/repertoire) when it runs, and
from Fractale's own scanning when it doesn't.

Made for Linux (KDE Plasma on Wayland); it also runs on macOS for
development.

## Build and run

```sh
cargo build --release
./target/release/fractale [PATH | file:///URI]
./target/release/fractale --select ~/Downloads/thing.zip   # its folder, it selected
```

A file given as PATH opens its folder with the file selected. With Fractale
already running, a second launch hands the request to the running window
and exits (Linux, over the session bus).

The Nix package needs the same runtime libraries as Sentinelle (libGL,
wayland, libxkbcommon, the X11 libraries); see
[docs/HANDOFF.md](docs/HANDOFF.md). `dist/` has the desktop entry
(`MimeType=inode/directory`) and a D-Bus activation file for
`org.freedesktop.FileManager1`.

## How it uses répertoire

When the répertoire daemon runs, folder sizes and search results come from
its index. They're instant and stay live as files change, and the status
bar shows the index state (*live*, *scanning N%*, …). Folders it doesn't
index, and everything when it isn't running, are measured by Fractale
itself, with the same rules (allocated bytes, hard links counted once,
mounts not crossed). Fractale switches between the two without the numbers
blinking, and looks for the daemon again every minute. With the daemon,
"biggest folders here" lists the largest folders anywhere below the open
one.

## Previews

- **Images:** PNG, JPEG, WebP, GIF (animated), TIFF, BMP and the like.
- **GIMP:** XCF.
- **Your own formats:** Pixelle sprites, terrains and palettes;
  Orchestre songs (as a piano roll) and sound effects (as a waveform);
  Prisme documents (their embedded preview).
- **Audio:** waveforms.
- **Documents:** PDFs (first page), SVGs, office files, HTML pages and
  fonts.
- **Videos:** a frame with `ffmpeg`.
- **Everything else on Linux:** any installed freedesktop thumbnailer.

Rest the pointer on a video, an audio file or an Orchestre file for a
moment and it plays (videos in the tile, with their sound); move away to stop. The
wait is 200 ms; `FRACTALE_HOVER_DELAY_MS` changes it.

Previews go into the shared thumbnail cache (`~/.cache/thumbnails`), so
other file managers use them too. Each format is a small `Previewer`
(`src/model/thumbs/previewers/`); adding a format means adding one.

## Keyboard

| Keys | |
|---|---|
| Arrows, Home/End, Page Up/Down | move (Shift extends the selection) |
| Tab | switch between folders and files |
| Enter | open (folders in Fractale, files with their app) |
| Right / Left (folders) | expand / collapse a row |
| Space (folders) | peek: the folder's files on the right, without opening it |
| Backspace, Alt+Up | parent folder |
| Alt+Left / Alt+Right, mouse back/forward | history |
| Typing a name | jump to it |
| Ctrl+L | edit the path (Tab completes) |
| Ctrl+F | search (here, or everywhere) |
| Ctrl+A, Escape | select all, clear |
| Ctrl+C / Ctrl+X / Ctrl+V | copy / cut / paste files |
| Ctrl+Shift+C | copy the path as text |
| F2 | rename |
| Ctrl+Shift+N | new folder |
| Delete / Shift+Delete | move to trash / delete permanently (asks) |
| Ctrl+H | show hidden files |
| F5 | refresh |
| Ctrl+wheel | preview size |

In search results, Enter opens a result and Alt+Enter shows it in its
folder.

Dragging files onto a folder row moves them there, or copies them if the
folder is on another disk. Shift forces a move, Ctrl a copy.

## Development

```sh
cargo test                       # everything (UI tests are headless, egui_kittest)
scripts/vm.sh test               # the same in the Linux VM, as a user then as root
scripts/fixtures.sh              # test trees, incl. 5,000 images, in target/fixtures
cargo run --example preview -- OUT 256 FILE…   # render previews to look at
```

Linux checks run in a Lima VM shared with répertoire (`scripts/vm.sh`).

### Testing the clipboard and drag and drop

`scripts/e2e/run.sh` checks the Wayland clipboard and drag and drop against
the real binary in a headless KWin, inside the VM:

```sh
scripts/vm.sh run scripts/e2e/run.sh                     # KWin 6 if set up (below), else the system KWin
scripts/vm.sh run KWIN=kwin_wayland scripts/e2e/run.sh   # Ubuntu's KWin 5.27
```

It opens `/tmp/fe2e/open`, then copies, cuts and pastes with `wl-clipboard`,
and drags files in and out with `scripts/e2e/peer`. The peer is a small
Wayland client that plays the other app, and drives the pointer and keyboard
through KWin's fake-input protocol. Window rules put the peer at (0, 0) and
Fractale at (400, 0) (`scripts/e2e/kwin-session.sh`), and the clicks use fixed
coordinates for a 1400×900 window. Each check prints PASS or FAIL; logs stay
in `/tmp/fe2e/run`.

Needs `kwin-wayland`, `wl-clipboard` and `dbus-x11`
(`sudo apt-get install kwin-wayland wl-clipboard dbus-x11`). For KWin 6, the
version Plasma 6 ships, the VM uses nixpkgs:

```sh
sh <(curl -L https://nixos.org/nix/install) --no-daemon
nix-build '<nixpkgs>' -A kdePackages.kwin -o /tmp/kwin6
nix-build '<nixpkgs>' -A mesa -o /tmp/mesa-nix
```

KWin 6's headless backend needs a DRM render node, which the VM doesn't
have. Ubuntu's arm64 kernel doesn't ship `vgem`, so build it from the
kernel's source (`drivers/gpu/drm/vgem` at the running version) with
`make -C /lib/modules/$(uname -r)/build M=$PWD modules`, `insmod` it, then
`sudo chmod 666 /dev/dri/renderD128 /dev/dri/card0 /dev/udmabuf`. This
lasts until the VM reboots.

Measurements, known limits and what to check on the real machine are in
[docs/HANDOFF.md](docs/HANDOFF.md).
