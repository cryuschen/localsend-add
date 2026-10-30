# Desktop maintenance

The desktop fork is scoped to Linux CLI / GTK / GNOME / Nautilus. Preserve the
upstream license and copyright notices. Modified files carry a localsend-add
change notice; describe user-visible changes in CHANGELOG.desktop.md. Upstream
Flutter build instructions remain in UPSTREAM_README.md and CONTRIBUTING.md.

## Validation

```bash
cargo test --locked -p localsend --features full --jobs 2
cargo test --locked -p localsend-cli -p localsend-picker --jobs 2
python3 -m unittest discover -s support/desktop -p 'test_*.py'
bash support/gnome/test_picker.sh
gjs -m support/gnome/test_ring.mjs
gjs -m support/gnome/test_model.mjs
python3 support/desktop/check_publication.py
```

The GTK script uses its own Broadway display, D-Bus and temporary configuration.
The Rust tests use generated data and loopback peers. Do not run diagnostic
scripts against a real user's socket or change their installed binaries during
release work. Host firewall changes and real-device acceptance are separate
manual operations. The optional GNOME 50 headless-shell harness is
`bash support/gnome/test_picker_shell.sh`; it uses an isolated session.

## Release

Release tag format: `v1.18.2-desktop.N`. The CLI's Cargo package version stays
aligned with the upstream Flutter/Inno version as required by upstream CI;
the desktop release tag and CLI `LOCAL_VERSION` distinguish fork revisions.

The **Desktop preview release** workflow builds both binaries on Ubuntu 26.04,
runs desktop tests and packages the runtime assets. A matching tag push
publishes a GitHub prerelease; manual workflow dispatch builds artifacts by
default, with an explicit publish option. Review the resulting CI run before
announcing a preview. The workflow uses the repository's short-lived Actions
token; no personal token is stored in source.

To package already-built local binaries:

```bash
python3 support/desktop/check_receive.py target/release/localsend-cli --idle
python3 support/desktop/package.py --tag v1.18.2-desktop.1
python3 support/desktop/check_package.py release/*.tar.gz
```

Outputs go to ignored `release/`: an architecture-labelled tarball and
`SHA256SUMS`. Only an explicit runtime allowlist is packaged; configuration,
identity, pairing, Git metadata, logs and development backups are excluded.
This local package inherits the build host's library requirements. CI release
notes identify the build platform. Source releases work without Flutter SDK.

Before making the repository public, scan both current tracked content and
fork-specific historical blobs, inspect author-email exposure, verify that
release assets contain no personal state, and perform fake-home install,
upgrade, rollback and uninstall checks. Current automated checks are not a
substitute for clean-VM and real-device acceptance: publish as a preview until
those results exist. Never rewrite published history solely to hide a publicly
known author address; use a GitHub noreply address for new commits if desired.
