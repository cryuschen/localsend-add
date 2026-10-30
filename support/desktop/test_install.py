# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Installer lifecycle regressions in a fake home, without touching the desktop."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("desktop_install", Path(__file__).with_name("install.py"))
installer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = installer
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="localsend-portable-install-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.layout = installer.Layout(self.root / "home", self.root / "data", self.root / "state", self.root / "config")
        self.source = self.root / "arbitrary-clone-name"
        extension = self.source / "support/gnome" / installer.UUID
        extension.mkdir(parents=True)
        for name in ("extension.js", "metadata.json", "stylesheet.css"):
            (extension / name).write_text("fixture:" + name)
        menu = self.source / "support/nautilus/localsend_menu.py"
        menu.parent.mkdir(parents=True)
        menu.write_text("menu fixture")
        self.binaries = self.source / "bin"
        self.binaries.mkdir()
        for name in ("localsend-cli", "localsend-picker"):
            (self.binaries / name).write_text("binary fixture:" + name)
        self.settings = {"enabled-extensions": ["other-extension"], "disabled-extensions": []}
        self.running = False
        self.version = "localsend-cli 1.18.2-local-unified.5\n"

        def output(command, **kwargs):
            if command[-1] == "--version":
                return self.version
            if command[-2:] == ["status", "--json"]:
                return json.dumps({"running": self.running})
            self.fail(f"Unexpected command: {command}")

        def restore(before):
            for key, values in before.items():
                others = [v for v in self.settings[key] if v != installer.UUID]
                self.settings[key] = others + ([installer.UUID] if installer.UUID in values else [])

        for target, kwargs in (("check_dependencies", {}), ("settings", {"side_effect": lambda: {k: list(v) for k, v in self.settings.items()}}),
                               ("restore_membership", {"side_effect": restore})):
            patcher = patch.object(installer, target, **kwargs)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(installer.subprocess, "check_output", side_effect=output)
        patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(installer.subprocess, "run", return_value=subprocess.CompletedProcess([], 0))
        patcher.start(); self.addCleanup(patcher.stop)

    def install(self, **kwargs):
        installer.install(self.layout, self.source, self.binaries, **kwargs)

    def test_first_install_upgrade_rollback_and_uninstall_preserve_identity(self):
        self.layout.config.mkdir(parents=True)
        for name in ("identity.pem", "paired-v2.json", "config.toml"):
            (self.layout.config / name).write_text("private fixture " + name)
        original = {p.name: p.read_bytes() for p in self.layout.config.iterdir()}
        self.install()
        self.assertTrue(self.layout.cli.is_file())
        self.assertTrue(self.layout.cli.with_name("localsend").is_symlink())
        self.assertIn(installer.UUID, self.settings["enabled-extensions"])
        self.assertEqual(self.layout.cli.stat().st_mode & 0o777, 0o755)
        (self.binaries / "localsend-cli").write_text("updated fixture")
        self.install()
        self.assertEqual(self.layout.cli.read_text(), "updated fixture")
        # Other extension changes after installation must survive rollback/uninstall.
        self.settings["enabled-extensions"].append("added-later")
        installer.remove(self.layout, rollback=True)
        self.assertEqual(self.layout.cli.read_text(), "binary fixture:localsend-cli")
        installer.remove(self.layout)
        self.assertFalse(self.layout.cli.exists())
        self.assertFalse(self.layout.cli.with_name("localsend").is_symlink())
        self.assertEqual(self.settings["enabled-extensions"], ["other-extension", "added-later"])
        self.assertEqual({p.name: p.read_bytes() for p in self.layout.config.iterdir()}, original)

    def test_failed_install_restores_previous_files_and_keeps_manifest(self):
        self.install()
        before = self.layout.manifest.read_bytes()
        original_cli = self.layout.cli.read_bytes()
        (self.binaries / "localsend-cli").write_text("new backend")
        real_write = installer.atomic_write
        def fail_picker(path, data, mode=0o600):
            if path == self.layout.cli.with_name("localsend-picker"):
                raise OSError("simulated disk failure")
            return real_write(path, data, mode)
        with patch.object(installer, "atomic_write", side_effect=fail_picker):
            with self.assertRaisesRegex(OSError, "disk failure"):
                self.install()
        self.assertEqual(self.layout.cli.read_bytes(), original_cli)
        self.assertEqual(self.layout.manifest.read_bytes(), before)

    def test_busy_backend_or_changed_file_prevents_mutation(self):
        self.install()
        before = self.layout.manifest.read_bytes()
        self.running = True
        with self.assertRaisesRegex(RuntimeError, "running"):
            self.install()
        self.assertEqual(self.layout.manifest.read_bytes(), before)
        self.running = False
        self.layout.cli.write_text("manually changed")
        with self.assertRaisesRegex(RuntimeError, "outside the installer"):
            installer.remove(self.layout)
        self.assertEqual(self.layout.cli.read_text(), "manually changed")

    def test_existing_user_install_is_restored_after_uninstall(self):
        self.layout.cli.parent.mkdir(parents=True)
        self.layout.cli.write_text("original personal build")
        self.layout.cli.chmod(0o755)
        self.layout.cli.with_name("localsend").symlink_to("localsend-cli")
        self.install(cli_only=True)
        installer.remove(self.layout)
        self.assertEqual(self.layout.cli.read_text(), "original personal build")
        self.assertEqual(self.layout.cli.with_name("localsend").readlink(), Path("localsend-cli"))
        self.assertFalse((self.layout.data / "gnome-shell").exists())

    def test_check_is_read_only_and_unrelated_command_is_preserved(self):
        self.install(check=True)
        self.assertFalse(self.layout.state.exists())
        self.assertFalse(self.layout.home.exists())
        self.layout.cli.parent.mkdir(parents=True)
        alias = self.layout.cli.with_name("localsend")
        alias.write_text("another application")
        with self.assertRaisesRegex(RuntimeError, "unrelated 'localsend'"):
            self.install()
        self.assertEqual(alias.read_text(), "another application")
        self.assertFalse(self.layout.manifest.exists())

    def test_corrupted_backup_refuses_uninstall_before_mutating_files(self):
        self.layout.cli.parent.mkdir(parents=True)
        self.layout.cli.write_text("old backend")
        self.install(cli_only=True)
        manifest = json.loads(self.layout.manifest.read_text())
        Path(manifest["files"][0]["backup"]).write_text("corrupt")
        current = self.layout.cli.read_bytes()
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            installer.remove(self.layout)
        self.assertEqual(self.layout.cli.read_bytes(), current)


if __name__ == "__main__":
    unittest.main()
