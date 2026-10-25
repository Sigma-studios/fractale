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

## To check on the target machine

1. Frame times at 144 Hz with a big folder (`FRACTALE_FRAME_STATS=1
   fractale ~/some/10k-file/folder` prints the worst frame each second).
2. With répertoire running as the system service: the badge says *live*,
   bars move while a big download or build runs in a subfolder, and
   `systemctl stop repertoire` falls back to *own scan* without the bars
   blinking.
3. The fallback's priority: measuring `~` (520k files) without répertoire
   must not make the desktop or a game stutter (nice 19, idle I/O class).
