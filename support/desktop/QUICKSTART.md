# Installation and recovery

LocalSend Add is an unofficial GNOME desktop derivative of LocalSend, released as a preview. Supported desktop target: Ubuntu 26.04, GNOME Shell 50, Nautilus 50, GTK 4.10 or newer. The sender communicates with official LocalSend clients using protocol v2. Other GNOME versions are not supported by the extension. Use `--cli-only` for a backend-only installation.

## Install a release

1. Download a `.tar.gz` archive and `SHA256SUMS` from [this repository's releases](https://github.com/cryuschen/localsend-add/releases).
2. In the download directory, verify the downloaded archive: `sha256sum --ignore-missing -c SHA256SUMS`. Checksums detect download corruption; obtain both files from the trusted release page.
3. Extract the archive and open a terminal in the extracted directory.
4. On Ubuntu 26.04 install the runtime dependencies:

   ```bash
   sudo apt install python3 python3-gi python3-nautilus gir1.2-nautilus-4.1 gir1.2-gtk-4.0
   ```

5. Run the following **without sudo**:

   ```bash
   python3 support/desktop/install.py --check
   python3 support/desktop/install.py
   ```

6. Log out and back in to load both desktop integrations. The backend starts when requested; no login autostart is created.
7. Make sure `~/.local/bin` is on PATH. Use `~/.local/bin/localsend-cli start` if your terminal has not picked it up yet.

The installer refuses an active backend, unrelated command symlinks and files changed outside the installer. If upgrading, finish transfers and run `localsend stop` first. It never stops a running transfer on your behalf. Do not run the historical `install_panel.py` or `install_local.py` for a fresh installation.

The release is dynamically linked; use source builds if your Linux libraries are older than the release build platform. The release notes identify that platform and architecture. There is no automatic download or upgrade service.

## Source build

Install Rust with rustup and the OS development dependencies (`build-essential`, `pkg-config`, `libgtk-4-dev`, `libssl-dev` on Ubuntu). From any clone directory:

```bash
cargo build --release --locked -p localsend-cli -p localsend-picker --jobs 2
python3 support/desktop/install.py --check
python3 support/desktop/install.py
```

The repository's `rust-toolchain.toml` pins Rust. Flutter and the Flutter submodule are not needed for these two binaries. First-time builds need network access; `--offline` is only appropriate after dependencies have been cached.

## Paths and personal state

| Purpose | Default location |
|---|---|
| Binaries and `localsend` alias | `~/.local/bin/` |
| GNOME extension | `~/.local/share/gnome-shell/extensions/localsend-transfer@local/` |
| Nautilus menu | `~/.local/share/nautilus-python/extensions/localsend_menu.py` |
| Device configuration, private identity and pairing | `~/.config/localsend-cli/` |
| Install records and recoverable backups | `~/.local/state/localsend-add/` |

`XDG_DATA_HOME`, `XDG_CONFIG_HOME` and `XDG_STATE_HOME` are respected; binaries remain in `~/.local/bin` to match the desktop launchers. Identity and pairing are generated/loaded by the backend, independently for each user. The installer does not copy personal state or set a machine-specific alias, port or download directory.

## Using and troubleshooting

Connect both devices to the same reachable Wi-Fi/hotspot and open LocalSend on the receiving device. Guest Wi-Fi isolation and sleeping iOS apps can prevent discovery. Default TCP/UDP port: `53317`. The official Flutter desktop app and this backend must not share the same listening port. Run only one, or set a different CLI port (for example `53318`) in its generated `config.toml` and allow that port through your firewall.

Paired devices auto-accept files; only pair trusted devices. Unknown requests expire after four seconds. Ordinary transfers use HTTPS and validate saved peer identity. Optional browser-link mode uses unencrypted HTTP and is not suitable for sensitive files. Directory contents are received with flattened file names; empty folders are omitted.

Receive files are published after verification without overwriting an existing file. The receive filesystem must support hard links (for example ext4 or Btrfs); unsupported filesystems produce an explicit failure. An abandoned accepted session is discarded after approximately two idle minutes; continuing transfers have no overall duration limit.

Useful commands:

```bash
localsend start
localsend status --json
localsend devices --json
localsend send --to "Device name" /path/to/file
localsend send --to "Device name" --text "Hello"
localsend unpair "Device name"
localsend stop
```

Log: `~/.config/localsend-cli/background.log`. Before sharing logs, remove personal filenames, addresses and device names. Never share `identity.pem` or `paired-v2.json`.

## Upgrade, rollback and uninstall

After stopping the backend, run the installer from the new release. It retains the files which existed before the first managed installation, so uninstalling an upgrade restores your original installation rather than discarding it.

```bash
python3 support/desktop/install.py --rollback
python3 support/desktop/install.py --uninstall
```

Rollback undoes the latest install/upgrade. Uninstall removes managed files or restores the originals. Both keep configuration, identity, pairing and backups. A checksum mismatch or outside modification stops removal before any owned file is changed. Desktop changes require a fresh login. Other GNOME extension settings are preserved.

Do not delete the installation records/backups while you still need rollback or restoration. Source checkouts and downloaded release archives can be removed after installation if you keep a copy of the installer for maintenance.
