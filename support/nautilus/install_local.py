"""Install a verified local build and the menu, retaining a recoverable backup."""

from datetime import datetime
from zoneinfo import ZoneInfo
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def replace_file(source, destination, mode):
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix="." + destination.name + ".", dir=destination.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(source.read_bytes())
            output.flush()
            os.fsync(output.fileno())
            os.fchmod(output.fileno(), mode)
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


binary = Path(sys.argv[1]).resolve()
version = subprocess.check_output([binary, "--version"], text=True).strip()
assert version == "localsend-cli 1.18.2-local-menu.3", version
home = Path.home()
target = home / ".local/bin/localsend-cli"
shortcut = home / ".local/bin/localsend"
extension = home / ".local/share/nautilus-python/extensions/localsend_menu.py"
menu_source = Path(__file__).with_name("localsend_menu.py")
assert not target.is_symlink(), "Unexpected CLI symlink; inspect before installing"
assert shortcut.is_symlink() and shortcut.resolve() == target.resolve(), "Unexpected localsend shortcut"
config = home / ".config/localsend-cli"
config_hashes = {p.name: digest(p) for p in (config / "config.toml", config / "paired-v2.json", config / "identity.pem")}
backup = home / ".local/state/localsend-cli/backups" / datetime.now(ZoneInfo("Asia/Shanghai")).strftime("%Y%m%d-%H%M%S-before-menu")
backup.mkdir(parents=True, mode=0o700)
backup.chmod(0o700)
manifest = {"version": version, "files": [], "preserved_config": config_hashes}
for path in (target, extension):
    assert not path.is_symlink(), f"Unexpected symlink: {path}"
    previous = digest(path) if path.exists() else None
    record = {"path": str(path), "original_sha256": previous, "backup": path.name}
    if path.exists():
        shutil.copy2(path, backup / path.name)
    manifest["files"].append(record)
for name in config_hashes:
    shutil.copy2(config / name, backup / name)

applied = []
try:
    for source, destination, mode in ((binary, target, 0o755), (menu_source, extension, 0o644)):
        entry = next(e for e in manifest["files"] if e["path"] == str(destination))
        assert (digest(destination) if destination.exists() else None) == entry["original_sha256"], "Concurrent installation change"
        replace_file(source, destination, mode)
        applied.append(entry)
        entry["installed_sha256"] = digest(destination)
except BaseException:
    for entry in reversed(applied):
        path = Path(entry["path"])
        if entry["original_sha256"]:
            replace_file(backup / entry["backup"], path, (backup / entry["backup"]).stat().st_mode & 0o777)
        elif path.exists():
            path.unlink()
    raise
assert all(digest(config / name) == value for name, value in config_hashes.items())
(backup / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
rollback = '''from pathlib import Path
import hashlib, json, os, subprocess, tempfile
root = Path(__file__).resolve().parent
data = json.loads((root / "manifest.json").read_text())
cli = Path(data["files"][0]["path"])
result = subprocess.run([str(cli), "stop"], capture_output=True, text=True)
if result.returncode:
    raise SystemExit("Stop the current CLI first: " + result.stderr)
for item in data["files"]:
    path = Path(item["path"])
    if not path.exists() or hashlib.sha256(path.read_bytes()).hexdigest() != item["installed_sha256"]:
        raise SystemExit("Installed file changed; inspect before rollback: " + str(path))
for item in data["files"]:
    path = Path(item["path"])
    if item["original_sha256"] is None:
        path.unlink()
        continue
    saved = root / item["backup"]
    if hashlib.sha256(saved.read_bytes()).hexdigest() != item["original_sha256"]:
        raise SystemExit("Backup checksum mismatch: " + str(saved))
    fd, name = tempfile.mkstemp(prefix=".rollback-", dir=path.parent)
    with os.fdopen(fd, "wb") as out:
        out.write(saved.read_bytes())
        os.fchmod(out.fileno(), saved.stat().st_mode & 0o777)
    os.replace(name, path)
print("Original CLI restored; menu extension restored/removed. Reload Nautilus manually.")
print("Your current identity and receive settings were preserved.")
'''
(backup / "rollback.py").write_text(rollback)
compile(rollback, str(backup / "rollback.py"), "exec")
print("Installed:", version)
print("Binary:", target, binary.stat().st_size, "bytes")
print("Menu:", extension)
print("Backup:", backup)
print("Rollback: python3", backup / "rollback.py")
print("Configuration, identity and paired devices preserved. No autostart was added.")
