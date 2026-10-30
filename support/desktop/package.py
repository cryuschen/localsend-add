#!/usr/bin/env python3
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Package an explicit desktop runtime allowlist, plus dependency license texts.

Copyright 2026 localsend-add contributors. SPDX-License-Identifier: Apache-2.0
"""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = ["README.md", "LICENSE", "NOTICE", "SECURITY.md", "CHANGELOG.desktop.md",
           "support/desktop/install.py", "support/desktop/QUICKSTART.md",
           "support/desktop/screenshots/text-sender.png",
           "support/nautilus/localsend_menu.py"]
EXTENSION_FILES = ["extension.js", "metadata.json", "model.mjs", "ring.mjs", "rpc.mjs", "send_dialog.py", "stylesheet.css"]


def dependency_notices(destination, target):
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", target
    ], cwd=ROOT, text=True))
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {p["id"]: p for p in metadata["resolve"]["nodes"]}
    pending = [p["id"] for p in packages.values() if p["name"] in {"localsend-cli", "localsend-picker"} and p["source"] is None]
    visited = set()
    while pending:
        identifier = pending.pop()
        if identifier in visited:
            continue
        visited.add(identifier)
        pending.extend(nodes[identifier]["dependencies"])
    lines = ["# Bundled Rust dependency notices", "", "Generated from the locked Cargo dependency graph for " + target + ".",
             "System shared libraries are supplied separately by the operating system.", ""]
    missing = []
    for identifier in sorted(visited, key=lambda i: (packages[i]["name"], packages[i]["version"])):
        package = packages[identifier]
        if package["source"] is None:
            continue
        name = package["name"] + "-" + package["version"]
        source = Path(package["manifest_path"]).parent
        texts = {p for p in source.iterdir() if p.is_file() and p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))}
        for directory in (source / "LICENSES", source / "licenses"):
            if directory.is_dir():
                texts.update(p for p in directory.rglob("*") if p.is_file())
        if package.get("license_file"):
            license_file = source / package["license_file"]
            if license_file.is_file():
                texts.add(license_file)
        lines += [f"## {name}", "", "License expression: " + (package.get("license") or "See included license file"),
                  "Source: " + (package.get("repository") or package.get("homepage") or f"https://crates.io/crates/{package['name']}/{package['version']}"), ""]
        if not texts:
            supplemental = ROOT / "support/desktop/licenses" / name
            texts = set(supplemental.glob("LICENSE*")) if supplemental.exists() else set()
        if not texts:
            missing.append(name)
        for text in sorted(texts):
            relative = text.relative_to(source) if text.is_relative_to(source) else Path(text.name)
            target_path = destination / "licenses" / name / relative
            target_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(text, target_path)
            lines.append(f"- [License text: {relative}](licenses/{name}/{relative})")
        lines.append("")
    (destination / "THIRD_PARTY_NOTICES.md").write_text("\n".join(lines))
    if missing:
        raise RuntimeError("Dependency license texts missing; review before publication: " + ", ".join(missing))


def package(binary_dir, output, tag, architecture):
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+-desktop\.[0-9]+", tag):
        raise ValueError("Expected a version such as v1.18.2-desktop.1")
    targets = {"x86_64": "x86_64-unknown-linux-gnu", "aarch64": "aarch64-unknown-linux-gnu"}
    if architecture not in targets:
        raise ValueError("Supported release architectures: x86_64, aarch64")
    output.mkdir(parents=True, exist_ok=True)
    name = f"localsend-add-{tag}-linux-{architecture}"
    with tempfile.TemporaryDirectory(prefix="localsend-package-") as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        for relative in RUNTIME + [f"support/gnome/localsend-transfer@local/{name}" for name in EXTENSION_FILES]:
            origin = ROOT / relative
            if not origin.is_file() or origin.is_symlink():
                raise RuntimeError(f"Missing or unexpected package input: {relative}")
            target = stage / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(origin, target)
        for binary in ("localsend-cli", "localsend-picker"):
            origin = binary_dir / binary
            header = origin.read_bytes()[:20]
            expected_machine = {"x86_64": 62, "aarch64": 183}[architecture]
            if header[:4] != b"\x7fELF" or struct.unpack("<H", header[18:20])[0] != expected_machine:
                raise RuntimeError(f"Wrong ELF architecture for {binary}: expected {architecture}")
            subprocess.run([str(origin.resolve()), "--version"], check=True, capture_output=True)
            target = stage / "bin" / binary
            target.parent.mkdir(exist_ok=True)
            shutil.copyfile(origin, target)
            target.chmod(0o755)
        dependency_notices(stage, targets[architecture])
        manifest = {str(p.relative_to(stage)): hashlib.sha256(p.read_bytes()).hexdigest()
                    for p in sorted(stage.rglob("*")) if p.is_file()}
        (stage / "FILES.sha256").write_text("".join(f"{value}  {path}\n" for path, value in manifest.items()))
        archive = output / (name + ".tar.gz")
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(stage, arcname=name)
    archives = sorted(output.glob("localsend-add-v*-desktop.*-linux-*.tar.gz"))
    (output / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in archives))
    print(archive)
    print(output / "SHA256SUMS")
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "target/release")
    parser.add_argument("--output", type=Path, default=ROOT / "release")
    parser.add_argument("--arch", default=platform.machine())
    arguments = parser.parse_args()
    package(arguments.binary_dir, arguments.output, arguments.tag, arguments.arch)
