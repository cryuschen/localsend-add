#!/usr/bin/env python3
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Check publishable content without printing secret values. Apache-2.0."""
import argparse
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
PATTERNS = {
    "private key": re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----\r?\n[A-Za-z0-9+/=\r\n]+-----END"),
    "GitHub token": re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})"),
    "provider API key": re.compile(rb"sk-(?:proj-)?[A-Za-z0-9_-]{40,}"),
    "AWS access ID": re.compile(rb"AKIA[A-Z0-9]{16}"),
    "personal home path": re.compile(rb"/home/[a-zA-Z0-9_.-]+/"),
}
PRIVATE_NAMES = {"identity.pem", "config.toml", "paired-v2.json", "background.log", "control.sock"}


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def inspect(label, data):
    failures = []
    if b"\0" in data:
        return failures
    for kind, pattern in PATTERNS.items():
        for match in pattern.finditer(data):
            line = data[:match.start()].count(b"\n") + 1
            failures.append(f"{label}:{line}: {kind}")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", action="store_true", help="also scan changed historical blobs since UPSTREAM_BASE")
    args = parser.parse_args()
    names = set(git("ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")) - {b""}
    failures = []
    scanned = 0
    for name in sorted(names):
        relative = Path(name.decode())
        path = ROOT / relative
        if not path.is_file() or path.is_symlink():
            continue
        if (relative.name in PRIVATE_NAMES and relative != Path(".cargo/config.toml")) or relative.name == ".env":
            failures.append(f"{relative}: private runtime filename is publishable")
        if path.stat().st_size <= 2_000_000:
            failures += inspect(str(relative), path.read_bytes())
            scanned += 1
    historical = 0
    if args.history:
        base = (ROOT / "support/desktop/UPSTREAM_BASE").read_text().strip()
        commits = git("rev-list", f"{base}..HEAD").splitlines()
        seen = set()
        for commit in commits:
            changed = set(git("diff", "--name-only", "-z", base, commit.decode()).split(b"\0"))
            for entry in git("ls-tree", "-r", "-z", commit.decode()).split(b"\0"):
                if not entry:
                    continue
                meta, name = entry.split(b"\t", 1)
                _, kind, oid = meta.split()
                if name not in changed or kind != b"blob" or oid in seen:
                    continue
                seen.add(oid)
                size = int(git("cat-file", "-s", oid.decode()))
                if size > 2_000_000:
                    continue
                data = git("cat-file", "blob", oid.decode())
                failures += inspect(f"{commit.decode()[:8]}:{name.decode()}", data)
                historical += 1
    if failures:
        print("Publication checks failed (matched values are intentionally hidden):", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"PASS: publication pattern checks on {scanned} candidate files and {historical} historical blobs.")
    print("Also review commit-author exposure and release contents; pattern checks are not an independent security audit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
