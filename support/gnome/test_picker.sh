#!/bin/bash
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
# Isolated display and D-Bus; never inject test devices into the daily backend.
set -eu
picker_source=$(cd -- "$(dirname -- "$0")/../.." && pwd)
picker_runtime=$(mktemp -d /tmp/localsend-picker-gtk-XXXXXX)
chmod 700 "$picker_runtime"
picker_server=''
cleanup() {
    if [ -n "$picker_server" ]; then kill "$picker_server" 2>/dev/null || true; wait "$picker_server" 2>/dev/null || true; fi
    rm -rf -- "$picker_runtime"
}
trap cleanup EXIT
mkdir -p "$picker_runtime/config" "$picker_runtime/data" "$picker_runtime/cache"
export XDG_RUNTIME_DIR="$picker_runtime" GSETTINGS_BACKEND=memory GTK_A11Y=none
export XDG_CONFIG_HOME="$picker_runtime/config" XDG_DATA_HOME="$picker_runtime/data" XDG_CACHE_HOME="$picker_runtime/cache"
export GDK_DEBUG=no-portals GIO_USE_VFS=local GIO_USE_VOLUME_MONITOR=unix
# Each test run owns its display socket and port, selected by broadwayd.
gtk4-broadwayd --port=0 :73 > "$picker_runtime/broadway.log" 2>&1 &
picker_server=$!
export GDK_BACKEND=broadway BROADWAY_DISPLAY=:73
for attempt in $(seq 1 100); do
    if [ -S "$picker_runtime/broadway74.socket" ]; then break; fi
    if ! kill -0 "$picker_server" 2>/dev/null; then cat "$picker_runtime/broadway.log"; exit 1; fi
    sleep 0.02
done
test -S "$picker_runtime/broadway74.socket"
cd -- "$picker_source"
dbus-run-session -- cargo test --locked --offline -p localsend-picker --jobs 2 -- --ignored --test-threads=1
dbus-run-session -- python3 -m unittest discover -s support/gnome -p 'test_*picker.py' -v
dbus-run-session -- python3 -m unittest discover -s support/gnome -p 'test_send_dialog.py' -v
dbus-run-session -- python3 -m unittest discover -s support/nautilus -p 'test_*.py' -v
