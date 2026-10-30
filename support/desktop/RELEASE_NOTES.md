Unofficial LocalSend GNOME desktop preview, based on upstream LocalSend 1.18.2.

**Desktop target:** Ubuntu 26.04, GNOME Shell 50, Nautilus 50, GTK >= 4.10.
**Binary build platform:** Ubuntu 24.04, Linux x86_64; dynamically linked.

Includes the shared Rust backend, native GTK file/folder/text sender, GNOME
status ring, Nautilus menu and user-local installer. CLI receive files are
verified and published without overwriting, with cancellation and idle cleanup.

Download the archive and SHA256SUMS, verify with
`sha256sum --ignore-missing -c SHA256SUMS`, then extract and follow README.md.
Install runtime dependencies first, run
`python3 support/desktop/install.py --check`, then the same command without
`--check`. Do not run the installer with sudo. Stop an existing backend before
upgrading. Log out/in to load desktop components.

Configuration, identity and pairing are generated for each user and preserved
across upgrades/removal. Rollback/uninstall commands are in the installation
guide. No personal configuration is included in the archive.

This is a preview: isolated loopback/GTK/installer regressions are covered;
clean-VM and real-phone acceptance are not yet complete. Received directory
contents are flattened; empty folders are omitted. Atomic receive publication
requires filesystem hard-link support. Browser-link mode is unencrypted HTTP.
Other GNOME versions and platforms are outside this desktop preview's scope.
