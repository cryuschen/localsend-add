#!/usr/bin/env python3
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Verify a real release archive and its complete lifecycle in a fake home."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tarfile
import tempfile
from unittest.mock import patch


def verify(archive):
    with tempfile.TemporaryDirectory(prefix="localsend-package-smoke-") as temporary:
        root = Path(temporary)
        with tarfile.open(archive) as tar:
            members = tar.getmembers()
            assert all(not m.name.startswith("/") and ".." not in Path(m.name).parts
                       and (m.isfile() or m.isdir()) for m in members)
            assert not any(Path(m.name).name in {"identity.pem", "config.toml", "paired-v2.json", ".env", "control.sock"} for m in members)
            tar.extractall(root / "extracted", filter="data")
        source, = (root / "extracted").iterdir()
        for line in (source / "FILES.sha256").read_text().splitlines():
            expected, relative = line.split("  ", 1)
            assert hashlib.sha256((source / relative).read_bytes()).hexdigest() == expected, relative
        assert (source / "THIRD_PARTY_NOTICES.md").is_file()
        spec = importlib.util.spec_from_file_location("package_smoke_installer", source / "support/desktop/install.py")
        installer = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = installer
        spec.loader.exec_module(installer)
        xdg = root / "xdg"
        layout = installer.Layout(root / "home", xdg / "data", xdg / "state/localsend-add", xdg / "config/localsend-cli")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        environment = {"XDG_CONFIG_HOME": str(xdg / "config"), "XDG_DATA_HOME": str(xdg / "data"),
                       "XDG_STATE_HOME": str(xdg / "state"), "LOCALSEND_PORT": str(port),
                       "LOCALSEND_ALIAS": "package-smoke", "LOCALSEND_DESTINATION": str(root / "received"),
                       "LOCALSEND_NO_NOTIFICATIONS": "1"}
        # Exercise real binaries and filesystem operations. Settings alone are
        # in-memory: the smoke must never change the user's GNOME session.
        with patch.dict(os.environ, environment), patch.object(installer, "check_dependencies"), \
                patch.object(installer, "settings", return_value={"enabled-extensions": [], "disabled-extensions": []}), \
                patch.object(installer, "restore_membership"):
            installer.install(layout, source, source / "bin", enable=False)
            try:
                result = subprocess.run([str(layout.cli), "start", "--json"], check=True, capture_output=True, text=True, timeout=20)
                assert json.loads(result.stdout)["running"] is True
                identity = layout.config / "identity.pem"
                assert identity.is_file() and identity.stat().st_mode & 0o777 == 0o600
                fingerprint = hashlib.sha256(identity.read_bytes()).hexdigest()
            finally:
                subprocess.run([str(layout.cli), "stop", "--force"], check=True, capture_output=True, timeout=20)
            installer.install(layout, source, source / "bin", enable=False)
            installer.remove(layout, rollback=True)
            installer.remove(layout)
            assert not layout.cli.exists()
            assert not layout.cli.with_name("localsend").is_symlink()
            assert hashlib.sha256(identity.read_bytes()).hexdigest() == fingerprint
            assert not (layout.config / "control.sock").exists()
        print("PASS: archive checksums/allowlist, real first start with private identity, upgrade, rollback and uninstall; personal state retained")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    verify(parser.parse_args().archive)
