#!/bin/bash
set -eu
picker_source=$(cd -- "$(dirname -- "$0")/../.." && pwd)
picker_temporary=$(mktemp -d /tmp/localsend-picker-shell-XXXXXX)
cleanup() {
    # The isolated Shell may autostart the document portal. Unmount only this
    # test's FUSE directories before removing its temporary configuration.
    for mount in "$picker_temporary/runtime/doc" "$picker_temporary/runtime/gvfs"; do
        if mountpoint -q "$mount"; then fusermount3 -u "$mount" 2>/dev/null || true; fi
    done
    for attempt in $(seq 1 30); do
        if rm -rf -- "$picker_temporary" 2>/dev/null; then return; fi
        sleep 0.1
    done
    rm -rf -- "$picker_temporary"
}
trap cleanup EXIT
mkdir -p "$picker_temporary/config" "$picker_temporary/data" "$picker_temporary/cache" "$picker_temporary/runtime"
chmod 700 "$picker_temporary/runtime"
picker_preview="$picker_source/../development-setup/rust-picker-preview"
mkdir -p "$picker_preview"
env XDG_CONFIG_HOME="$picker_temporary/config" XDG_DATA_HOME="$picker_temporary/data" \
    XDG_CACHE_HOME="$picker_temporary/cache" XDG_RUNTIME_DIR="$picker_temporary/runtime" \
    GSETTINGS_BACKEND=memory GTK_A11Y=none GDK_BACKEND=wayland GDK_DEBUG=no-portals \
    GIO_USE_VFS=local GIO_USE_VOLUME_MONITOR=unix LS_UI_OUTPUT="$picker_preview" \
    LS_TEST_CLI="${LS_TEST_CLI:-$picker_source/target/release/localsend-cli}" \
    LS_TEST_PICKER="${LS_TEST_PICKER:-$picker_source/target/release/localsend-picker}" \
    dbus-run-session -- gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x900 \
    --automation-script="$picker_source/support/gnome/check_picker_shell.mjs"
