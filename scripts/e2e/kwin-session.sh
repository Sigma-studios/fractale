#!/usr/bin/env bash
# Start a headless KWin for the e2e tests, with window rules that put the
# test peer at (0,0) and Fractale at (400,0), both without decorations.
# Usage: scripts/e2e/kwin-session.sh start|stop
#   KWIN=path/to/kwin_wayland   which KWin (default: the system one)
#   KWIN_ENV="VAR=value ..."    extra environment for KWin only, e.g. nixpkgs
#                               Mesa paths for a nix-built KWin (run.sh sets them)
set -euo pipefail
RUN=/tmp/fe2e/run
SOCK=wl-fe2e
case "${1:-start}" in
start)
    mkdir -p "$RUN/config"
    cat > "$RUN/config/kwinrulesrc" <<'RULES'
[General]
count=2
rules=peer,demo

[peer]
Description=dnd peer
wmclass=dnd-peer
wmclassmatch=1
position=0,0
positionrule=2
noborder=true
noborderrule=2

[demo]
Description=fractale
wmclass=fractale
wmclassmatch=1
position=400,0
positionrule=2
noborder=true
noborderrule=2
RULES
    # Fake input is a restricted interface; this lifts the check.
    # KWIN_ENV: extra environment for KWin only (e.g. nix Mesa paths for a
    # nix-built KWin on a non-NixOS host); the demo keeps the system Mesa.
    # shellcheck disable=SC2086
    XDG_CONFIG_HOME="$RUN/config" KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 \
        env ${KWIN_ENV:-} setsid dbus-run-session -- "${KWIN:-kwin_wayland}" --virtual --no-lockscreen --no-global-shortcuts \
        --socket "$SOCK" --width 1920 --height 1080 > "$RUN/kwin.log" 2>&1 < /dev/null &
    echo $! > "$RUN/kwin.pid"
    for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$SOCK" ] && break; sleep 0.2; done
    [ -S "$XDG_RUNTIME_DIR/$SOCK" ] || { echo "kwin did not start"; cat "$RUN/kwin.log"; exit 1; }
    echo "WAYLAND_DISPLAY=$SOCK"
    ;;
stop)
    pkill -f "kwin_wayland.* --virtual.*$SOCK" 2>/dev/null || true
    [ -f "$RUN/kwin.pid" ] && kill "$(cat "$RUN/kwin.pid")" 2>/dev/null || true
    rm -f "$RUN/kwin.pid"
    ;;
esac
