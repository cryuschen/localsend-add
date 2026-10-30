# Desktop edition changes

## v1.18.2-desktop.1 — preview

- Unified on-demand CLI backend shared by the GNOME ring, Nautilus and terminal commands.
- GNOME Shell 50 status ring with real transfer progress, receive confirmation and cancellation.
- Native Rust / GTK4 sender for files, directories and UTF-8 multiline text.
- Device discovery, certificate-identified pairing, queued sends and optional browser links.
- CLI receive files are written to private temporary files, verified and atomically published without overwriting existing data; failed/cancelled data is removed.
- Concurrent incoming names are reserved separately; uploads are aborted when their session is cancelled.
- Idle accepted sessions and stalled upload bodies are reclaimed after approximately two minutes.
- User-local first-install/upgrade/check/rollback/uninstall workflow, preserving configuration and prior installations.
- Dedicated desktop CI, release bundles, checksums and publication checks.

Supported desktop target: Ubuntu 26.04 / GNOME Shell 50 / Nautilus 50 / GTK >= 4.10.
This preview has isolated loopback and GTK/menu regression coverage; clean-VM
and real-phone compatibility acceptance are not yet complete. Directory receives
flatten file names, and atomic receive publication requires hard-link support.
