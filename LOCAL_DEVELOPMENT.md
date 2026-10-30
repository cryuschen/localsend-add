# Local desktop development

This repository is the source checkout for the unofficial LocalSend Add desktop
edition. Its directory may have any name; it does not require a parent folder
containing `source/` or `development-setup/`.

- Public first-install/upgrade/rollback/uninstall: `support/desktop/install.py`.
- Build/test/release instructions: `support/desktop/MAINTAINING.md`.
- User documentation: `README.md` and `support/desktop/QUICKSTART.md`.
- Upstream reference: `support/desktop/UPSTREAM_README.md` and `CONTRIBUTING.md`.

The historical `support/gnome/install_panel.py` and
`support/nautilus/install_local.py` remain for old local upgrade/rollback
workflows. They are not portable installers. Existing external
`development-setup/` records are personal recovery material, not a build or
runtime dependency and must not be published.

Keep runtime identity, configuration and pairing outside the checkout.
`.gitignore` protects common private filenames but cannot remove secrets from
already-recorded history. Run the publication checker and inspect the staged
diff before committing. Do not upload compiler output, private install backups,
logs, or real user test data. Running tests/builds does not install the result;
never replace a user's active installation during release preparation.
