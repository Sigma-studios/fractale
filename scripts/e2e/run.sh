#!/usr/bin/env bash
# End-to-end tests of the clipboard and drag and drop with other apps, on the
# real Fractale binary in a headless KWin (see README "Testing the clipboard
# and drag and drop"). Inside the VM:
#
#   scripts/e2e/run.sh            KWin 6 from nixpkgs if set up, else the system KWin
#   KWIN=kwin_wayland scripts/e2e/run.sh
#
# The other app is wl-clipboard and `peer` (scripts/e2e/peer), which also
# drives the pointer and keyboard through KWin's fake-input protocol. Only
# /tmp/fe2e is touched. Prints PASS/FAIL per check; logs stay in /tmp/fe2e/run.
set -u
cd "$(dirname "$0")/../.."
REPO=$PWD
T=/tmp/fe2e
RUN=$T/run
FAILS=0
export WAYLAND_DISPLAY=wl-fe2e
unset DISPLAY

# KWin 6 (what Plasma 6 ships) from nixpkgs when present; see the README.
if [ -z "${KWIN:-}" ] && [ -x /tmp/kwin6/bin/kwin_wayland ]; then
    export KWIN=/tmp/kwin6/bin/kwin_wayland
    export KWIN_ENV="__EGL_VENDOR_LIBRARY_FILENAMES=/tmp/mesa-nix/share/glvnd/egl_vendor.d/50_mesa.json LIBGL_DRIVERS_PATH=/tmp/mesa-nix/lib/dri GBM_BACKENDS_PATH=/tmp/mesa-nix/lib/gbm"
fi
"${KWIN:-kwin_wayland}" --version

echo "building fractale and the peer"
(export CARGO_TARGET_DIR=$HOME/target-fractale CARGO_BUILD_JOBS=3; cargo build -q) || exit 1
(cd scripts/e2e/peer && CARGO_TARGET_DIR=$HOME/target-fractale-e2e CARGO_BUILD_JOBS=3 cargo build -q) || exit 1
FRACTALE=$HOME/target-fractale/debug/fractale
PEER=$HOME/target-fractale-e2e/debug/fractale-e2e-peer

cleanup() {
    [ -n "${APP:-}" ] && kill "$APP" 2>/dev/null
    pkill -x wl-copy 2>/dev/null
    "$REPO/scripts/e2e/kwin-session.sh" stop
}
trap cleanup EXIT

# The fixture. Folder rows sort by size, so the seeds keep inbox above target.
rm -rf "$T/open" "$T/elsewhere"
mkdir -p "$T/open/inbox" "$T/open/target" "$T/elsewhere" "$RUN"
for f in alpha beta gamma; do echo "$f" > "$T/open/$f.txt"; done
head -c 400000 /dev/zero > "$T/open/inbox/seed.bin"
head -c 200000 /dev/zero > "$T/open/target/seed.bin"
for f in zz-wlcopy zz-peercut zz-drop1 zz-drop2 zz-droprow zz-shift; do echo "$f" > "$T/elsewhere/$f.txt"; done

scripts/e2e/kwin-session.sh start >/dev/null || exit 1
"$FRACTALE" "$T/open" > "$RUN/fractale.out" 2> "$RUN/fractale.err" &
APP=$!
sleep 6

# Global coordinates: the window is at (400, 0) (KWin rule), 1400x900.
TILE1=935,150     # alpha.txt, first tile (other files sort after it)
TILE2=1067,150    # beta.txt
ROW_INBOX=470,104
ROW_TARGET=472,134
FILES=1300,600    # empty space in the files pane
SEARCH=1600,20
PEERWIN=150,150   # inside the peer's window at (0, 0)

ok() { echo "PASS $1"; }
bad() { echo "FAIL $1"; FAILS=$((FAILS + 1)); }
expect() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
act() { "$PEER" act "$@"; }
peer() { # log name, args...
    local name=$1; shift
    "$PEER" "$@" > "$RUN/peer-$name.log" 2>&1
    grep -v 'pointer enter' "$RUN/peer-$name.log" | sed 's/^/    peer| /'
}
listing() { (cd "$T" && find open elsewhere -type f ! -name seed.bin | sort | tr '\n' ' '); echo; }

echo; echo "== 1. Ctrl+C / Ctrl+X on a selected file -> wl-paste"
act click:$TILE1 key:ctrl+c wait:400
URI=$(wl-paste -n -t text/uri-list | tr -d '\r')
echo "    wl-paste -t text/uri-list: $URI"
expect "1a Ctrl+C puts the file on the system clipboard" '[ "$URI" = "file://$T/open/alpha.txt" ]'
act key:ctrl+x wait:400
TYPES=$(wl-paste --list-types | tr '\n' ' ')
CUT=$(wl-paste -n -t application/x-kde-cutselection)
echo "    types: $TYPES"
echo "    x-kde-cutselection: $CUT"
expect "1b Ctrl+X adds the KDE cut marker" '[ "$CUT" = 1 ]'

echo; echo "== 2. wl-copy a uri-list, Ctrl+V in Fractale -> copied"
printf 'file://%s/elsewhere/zz-wlcopy.txt\r\n' "$T" | wl-copy -t text/uri-list
act click:$FILES key:ctrl+v wait:1500
listing | sed 's/^/    files: /'
expect "2 pasted copy in the open folder, source kept" '[ -f $T/open/zz-wlcopy.txt ] && [ -f $T/elsewhere/zz-wlcopy.txt ]'
pkill -x wl-copy

echo; echo "== 3. the peer copies with KDE's cut marker, Ctrl+V -> moved"
"$PEER" copy-files --cut --via "$FILES" -- "$T/elsewhere/zz-peercut.txt" > "$RUN/peer-cut.log" 2>&1 &
PEERPID=$!
sleep 2.5
act key:ctrl+v wait:1500
wait $PEERPID
grep -v 'pointer enter' "$RUN/peer-cut.log" | sed 's/^/    peer| /'
listing | sed 's/^/    files: /'
expect "3 cut paste moved the file" '[ -f $T/open/zz-peercut.txt ] && [ ! -e $T/elsewhere/zz-peercut.txt ]'

echo; echo "== 4. the peer drops files on the file area, then on a folder row"
peer drop1 drag-in --via "$FILES" -- "$T/elsewhere/zz-drop1.txt" "$T/elsewhere/zz-drop2.txt"
sleep 1.5
expect "4a dropped files copied into the open folder" '[ -f $T/open/zz-drop1.txt ] && [ -f $T/open/zz-drop2.txt ] && [ -f $T/elsewhere/zz-drop1.txt ]'
peer drop2 drag-in --via "$FILES" --via "$ROW_TARGET" -- "$T/elsewhere/zz-droprow.txt"
sleep 1.5
listing | sed 's/^/    files: /'
expect "4b dropped on the target row lands in target/" '[ -f $T/open/target/zz-droprow.txt ] && [ ! -e $T/open/zz-droprow.txt ]'
# KWin 6 turns Shift into "move"; Fractale then moves rather than copies.
# KWin 5.27 only applies Shift when the target re-sends its actions, which
# Fractale doesn't do on every motion, so the drop stays a copy there.
if "${KWIN:-kwin_wayland}" --version | grep -q '^kwin 5'; then
    echo "SKIP 4c Shift while dropping moves (KWin 6 only)"
else
    peer drop3 drag-in --shift --via "$FILES" -- "$T/elsewhere/zz-shift.txt"
    sleep 1.5
    expect "4c Shift while dropping moves" '[ -f $T/open/zz-shift.txt ] && [ ! -e $T/elsewhere/zz-shift.txt ]'
fi

echo; echo "== 5. drag a tile out onto the peer"
peer out drag-out --action copy --via "$TILE1" --via "$PEERWIN"
expect "5a the peer received the URI" 'grep -qF "PEER DROP action=copy data=\"file://$T/open/alpha.txt\\r\\n\"" $RUN/peer-out.log'
expect "5b alpha.txt untouched (the target copies)" '[ -f $T/open/alpha.txt ]'
act click:$TILE2 key:ctrl+c wait:400
URI=$(wl-paste -n -t text/uri-list | tr -d '\r')
echo "    after the drag, click beta + Ctrl+C: $URI"
expect "5c not stuck: a click selects again" '[ "$URI" = "file://$T/open/beta.txt" ]'

echo; echo "== 6. drag a tile onto a folder row inside Fractale"
peer int drag-out --no-wait --via "$TILE1" --via "$ROW_INBOX"
sleep 1.5
listing | sed 's/^/    files: /'
N=$(find "$T" -name 'alpha*' | wc -l)
expect "6 alpha.txt moved into inbox/, once" '[ -f $T/open/inbox/alpha.txt ] && [ ! -e $T/open/alpha.txt ] && [ "$N" = 1 ]'

echo; echo "== 7. text: Ctrl+F, wl-copy hello, Ctrl+V in the search field"
act click:$FILES key:ctrl+f wait:300
wl-copy "hello"
# Clicking the field gives the window its focus back (wl-copy may borrow it).
# Then paste, type an x, and copy the whole field out: "hellox" can only
# come from the field.
act click:$SEARCH key:ctrl+v wait:800 key:x wait:200 key:ctrl+a key:ctrl+c wait:500
GOT=$(wl-paste -n)
echo "    search field copied back out: '$GOT'"
expect "7 the search field shows hello (then x typed)" '[ "$GOT" = hellox ]'

pkill -x wl-copy

echo; echo "== Fractale still running, its stderr (minus Mesa noise):"
kill -0 "$APP" && ok "fractale alive" || bad "fractale died"
grep -viE 'libEGL|MESA|ZINK|dri2|^$' "$RUN/fractale.err" | head -20 | sed 's/^/    | /'

echo
[ "$FAILS" = 0 ] && echo "ALL PASSED" || echo "$FAILS FAILED"
exit "$FAILS"
