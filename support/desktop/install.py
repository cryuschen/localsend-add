#!/usr/bin/env python3
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""User-local localsend-add installer. Copyright 2026 localsend-add contributors.

SPDX-License-Identifier: Apache-2.0
Never starts/stops a backend, installs system packages, or touches device identity.
"""
import argparse
import ast
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid
import fcntl

UUID = "localsend-transfer@local"
SOURCE = Path(__file__).resolve().parents[2]


def xdg(name, default):
    value = Path(os.environ.get(name, str(default)))
    return value if value.is_absolute() else default


@dataclass
class Layout:
    home: Path
    data: Path
    state: Path
    config: Path

    @classmethod
    def current(cls):
        home = Path.home()
        return cls(home, xdg("XDG_DATA_HOME", home / ".local/share"),
                   xdg("XDG_STATE_HOME", home / ".local/state") / "localsend-add",
                   xdg("XDG_CONFIG_HOME", home / ".config") / "localsend-cli")

    @property
    def cli(self):
        return self.home / ".local/bin/localsend-cli"

    @property
    def manifest(self):
        return self.state / "current.json"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def describe(path):
    if path.is_symlink():
        return {"link": os.readlink(path)}
    if path.is_file():
        return {"sha256": digest(path), "mode": path.stat().st_mode & 0o777}
    if path.exists():
        raise RuntimeError(f"Expected a file, found something else: {path}")
    return None


def atomic_write(path, contents, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(contents)
            stream.flush()
            os.fsync(stream.fileno())
            os.fchmod(stream.fileno(), mode)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def write_json(path, value):
    atomic_write(path, (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode())


def settings():
    return {key: ast.literal_eval(subprocess.check_output(
        ["gsettings", "get", "org.gnome.shell", key], text=True))
        for key in ("enabled-extensions", "disabled-extensions")}


def restore_membership(before):
    """Restore only our UUID; preserve changes to every other extension."""
    current = settings()
    for key, previous in before.items():
        values = [item for item in current[key] if item != UUID]
        if UUID in previous:
            values.append(UUID)
        subprocess.run(["gsettings", "set", "org.gnome.shell", key, repr(values)], check=True)


def ensure_stopped(layout):
    if not layout.cli.exists():
        return
    version = subprocess.check_output([str(layout.cli), "--version"], text=True).strip()
    if "-local-unified." not in version:
        raise RuntimeError("An unrelated localsend-cli is installed; move it aside before installing.")
    status = json.loads(subprocess.check_output([str(layout.cli), "status", "--json"], text=True))
    if status.get("running"):
        raise RuntimeError("The backend is running. Finish transfers, run 'localsend stop', then retry. No files changed.")


def check_dependencies(desktop):
    if sys.platform != "linux":
        raise RuntimeError("The desktop edition supports Linux only.")
    if os.geteuid() == 0:
        raise RuntimeError("Run this user-local installer as your regular user, without sudo.")
    if not desktop:
        return
    if shutil.which("gnome-shell") is None or shutil.which("gsettings") is None:
        raise RuntimeError("GNOME Shell 50 and gsettings are required; use --cli-only for the backend.")
    version = subprocess.check_output(["gnome-shell", "--version"], text=True).strip()
    if version.split()[-1].split(".")[0] != "50":
        raise RuntimeError(f"Supported desktop: GNOME Shell 50. Found {version}. Use --cli-only on other desktops.")
    try:
        import gi
        gi.require_version("Gtk", "4.0")
        gi.require_version("Nautilus", "4.1")
        from gi.repository import Gtk
        if (Gtk.get_major_version(), Gtk.get_minor_version()) < (4, 10):
            raise RuntimeError("GTK 4.10 or newer is required.")
    except (ImportError, ValueError) as error:
        raise RuntimeError("Install python3-gi, python3-nautilus, gir1.2-nautilus-4.1 and gir1.2-gtk-4.0.") from error
    settings()  # Check the logged-in user's settings before changing files.


def installation_plan(layout, source, binary_dir, cli_only):
    cli = binary_dir / "localsend-cli"
    version = subprocess.check_output([str(cli), "--version"], text=True).strip()
    if not version.startswith("localsend-cli 1.18.2-local-unified."):
        raise RuntimeError(f"Unexpected backend version: {version}")
    plan = [(cli, layout.cli, 0o755), (None, layout.cli.with_name("localsend"), None)]
    if not cli_only:
        picker = binary_dir / "localsend-picker"
        subprocess.run([str(picker), "--version"], check=True, capture_output=True, text=True)
        plan += [(picker, layout.cli.with_name("localsend-picker"), 0o755),
                 (source / "support/nautilus/localsend_menu.py",
                  layout.data / "nautilus-python/extensions/localsend_menu.py", 0o644)]
        extension = source / "support/gnome" / UUID
        plan += [(path, layout.data / "gnome-shell/extensions" / UUID / path.name, 0o644)
                 for path in sorted(extension.iterdir()) if path.is_file()]
    for origin, target, _ in plan:
        if origin is not None and not origin.is_file():
            raise RuntimeError(f"Missing package file: {origin}")
        if target.is_symlink() and (origin is not None or target.resolve() != layout.cli.resolve()):
            raise RuntimeError(f"Refusing to replace an unrelated symlink: {target}")
        if origin is None and target.exists() and not target.is_symlink():
            raise RuntimeError(f"Refusing to replace an unrelated 'localsend' command: {target}")
    return version, plan


def backup_record(path, directory, index):
    value = describe(path)
    record = {"path": str(path), "before": value}
    if value and "sha256" in value:
        saved = directory / str(index)
        shutil.copy2(path, saved)
        record["backup"] = str(saved)
    return record


def restore_record(record):
    path = Path(record["path"])
    before = record["before"]
    if before is None:
        path.unlink(missing_ok=True)
    elif "link" in before:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.unlink(missing_ok=True)
        path.symlink_to(before["link"])
    else:
        saved = Path(record["backup"])
        if digest(saved) != before["sha256"]:
            raise RuntimeError(f"Backup checksum mismatch: {saved}")
        atomic_write(path, saved.read_bytes(), before["mode"])


def validate_owned(records):
    for record in records:
        if describe(Path(record["path"])) != record["installed"]:
            raise RuntimeError(f"Installed file changed outside the installer: {record['path']}. No files changed.")


def install(layout, source, binary_dir, cli_only=False, enable=True, check=False):
    check_dependencies(not cli_only)
    version, plan = installation_plan(layout, source, binary_dir, cli_only)
    previous = json.loads(layout.manifest.read_text()) if layout.manifest.exists() else None
    if previous:
        validate_owned(previous["files"])
    ensure_stopped(layout)
    if check:
        print(json.dumps({"version": version, "files": [str(p) for _, p, _ in plan], "checks_passed": True}, indent=2))
        return
    layout.state.mkdir(parents=True, exist_ok=True, mode=0o700)
    layout.state.chmod(0o700)
    transaction = layout.state / "backups" / (datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8])
    transaction.mkdir(parents=True, mode=0o700)
    before_settings = settings() if not cli_only else None
    config_before = {str(p): describe(p) for p in (layout.config / name for name in ("config.toml", "identity.pem", "paired-v2.json"))}
    records = [backup_record(target, transaction, index) for index, (_, target, _) in enumerate(plan)]
    rollback = {"files": records, "settings": before_settings, "previous": previous}
    write_json(transaction / "rollback.json", rollback)
    owned = {item["path"]: item for item in previous["files"]} if previous else {}
    applied = []
    try:
        for (origin, target, mode), record in zip(plan, records):
            if describe(target) != record["before"]:
                raise RuntimeError(f"Concurrent installation change: {target}")
            if origin is None:
                target.parent.mkdir(parents=True, exist_ok=True)
                temporary = target.with_name(".localsend-" + uuid.uuid4().hex)
                try:
                    temporary.symlink_to("localsend-cli")
                    os.replace(temporary, target)
                finally:
                    temporary.unlink(missing_ok=True)
            else:
                atomic_write(target, origin.read_bytes(), mode)
            applied.append(record)
            original = owned.get(str(target), record)
            owned[str(target)] = {**original, "installed": describe(target)}
        if not cli_only and enable:
            restore_membership({"enabled-extensions": [UUID], "disabled-extensions": []})
        if {path: describe(Path(path)) for path in config_before} != config_before:
            raise RuntimeError("Device configuration changed concurrently; installation rolled back.")
        write_json(layout.manifest, {"version": version, "files": list(owned.values()),
                   "settings": previous["settings"] if previous and previous["settings"] is not None else before_settings,
                   "rollback": str(transaction / "rollback.json")})
    except BaseException:
        for record in reversed(applied):
            restore_record(record)
        if before_settings is not None:
            restore_membership(before_settings)
        raise
    print(f"Installed {version}. Backend remains stopped; no autostart was added.")
    print("Configuration, identity and paired devices preserved.")
    if not cli_only:
        print("Log out and back in to load the GNOME extension and Nautilus menu.")
    print(f"Installation record: {layout.manifest}")


def remove(layout, rollback=False):
    if not layout.manifest.exists():
        raise RuntimeError("No installation managed by this installer was found.")
    current = json.loads(layout.manifest.read_text())
    validate_owned(current["files"])
    ensure_stopped(layout)
    transaction = json.loads(Path(current["rollback"]).read_text()) if rollback else None
    records = transaction["files"] if transaction else current["files"]
    # Validate every backup before restoring any file.
    for record in records:
        if record["before"] and "sha256" in record["before"]:
            if digest(Path(record["backup"])) != record["before"]["sha256"]:
                raise RuntimeError(f"Backup checksum mismatch: {record['backup']}")
    before_settings = transaction["settings"] if transaction else current["settings"]
    if before_settings is not None:
        restore_membership(before_settings)
    for record in reversed(records):
        restore_record(record)
    if transaction and transaction["previous"]:
        write_json(layout.manifest, transaction["previous"])
    else:
        layout.manifest.unlink()
    print("Previous installation restored." if rollback else "Uninstalled managed files and restored any original files.")
    print("Personal configuration and backups retained. Log out/in to reload desktop components.")


@contextmanager
def installation_lock(layout):
    layout.state.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (layout.state / "install.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError("Another localsend-add installer is running.") from error
        yield


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli-only", action="store_true", help="install only the backend and command alias")
    parser.add_argument("--no-enable", action="store_true", help="install desktop files without enabling the extension")
    parser.add_argument("--binary-dir", type=Path, help="directory containing prebuilt binaries (default: bin/ in a release, otherwise target/release)")
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true", help="read-only dependency, version and conflict checks")
    action.add_argument("--uninstall", action="store_true", help="restore files which existed before the first installation")
    action.add_argument("--rollback", action="store_true", help="undo the most recent installation or upgrade")
    args = parser.parse_args()
    layout = Layout.current()
    try:
        def action():
            if args.uninstall or args.rollback:
                remove(layout, args.rollback)
                return
            binaries = args.binary_dir or (SOURCE / "bin" if (SOURCE / "bin").exists() else SOURCE / "target/release")
            install(layout, SOURCE, binaries, args.cli_only, not args.no_enable, args.check)
        if args.check:
            action()
        else:
            with installation_lock(layout):
                action()
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        parser.exit(1, f"Installation stopped: {error}\n")


if __name__ == "__main__":
    main()
