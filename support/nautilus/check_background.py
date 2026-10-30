# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Exercise isolated local peers, including real HTTPS directory transfer.

No personal configuration or files are used. Notifications are suppressed.
Usage: python3 support/nautilus/check_background.py target/debug/localsend-cli
"""

import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import sys
import tempfile
import time


binary = str(Path(sys.argv[1]).resolve())


def free_port():
    with socket.socket() as stream:
        stream.bind(("127.0.0.1", 0))
        return stream.getsockname()[1]


def eventually(function, timeout=35):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        result = function()
        if result:
            return result
        time.sleep(0.15)
    raise AssertionError("Timed out waiting for background result")


class Peer:
    def __init__(self, base, name):
        self.name = name
        self.root = base / name
        self.config = self.root / "localsend-cli"
        self.config.mkdir(parents=True)
        self.destination = base / (name + "-received")
        self.port = free_port()
        self.environment = {**os.environ, "XDG_CONFIG_HOME": str(self.root), "LOCALSEND_NO_NOTIFICATIONS": "1"}
        for variable in ("LOCALSEND_ALIAS", "LOCALSEND_PORT", "LOCALSEND_DESTINATION"):
            self.environment.pop(variable, None)
        key, cert = self.config / "key.tmp", self.config / "cert.tmp"
        subprocess.run([
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-noenc", "-days", "1",
            "-subj", "/CN=LocalSend-test", "-keyout", str(key), "-out", str(cert),
        ], check=True, capture_output=True)
        identity = self.config / "identity.pem"
        identity.write_bytes(key.read_bytes() + cert.read_bytes())
        identity.chmod(0o600)
        self.fingerprint = hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert.read_text())).hexdigest().upper()
        key.unlink()
        cert.unlink()
        (self.config / "config.toml").write_text(
            f"alias = {json.dumps(name)}\nport = {self.port}\ndestination = {json.dumps(str(self.destination))}\n"
        )

    def pair(self, other):
        (self.config / "paired-v2.json").write_text(json.dumps({
            "version": 1, "devices": {other.fingerprint: {
                "alias": other.name,
                "channels": [{"host": "127.0.0.1", "port": other.port, "protocol": "HTTPS"}],
            }},
        }))

    def run(self, *arguments, check=True):
        result = subprocess.run([binary, *arguments], env=self.environment,
                                capture_output=True, text=True, timeout=20)
        if check and result.returncode:
            raise AssertionError(f"{arguments}: {result.stderr}\n{self.log()}")
        return result

    def data(self, *arguments):
        if arguments and arguments[0] == "start" and "--json" not in arguments:
            arguments = (*arguments, "--json")
        return json.loads(self.run(*arguments).stdout)

    def rpc(self, value):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(5)
            stream.connect(str(self.config / "control.sock"))
            stream.sendall((json.dumps(value) + "\n").encode())
            return json.loads(stream.makefile("r").readline())

    def menu(self, session="integration-menu"):
        return self.rpc({"command": "menu_poll", "session": session})

    def log(self):
        p = self.config / "background.log"
        return p.read_text()[-4000:] if p.exists() else ""

    def stop(self):
        self.run("stop", "--force", check=False)


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="ls-menu-") as directory:
        base = Path(directory)
        sender, receiver = Peer(base, "menu-sender"), Peer(base, "menu-receiver")
        observer = Peer(base, "unpaired-neighbor")
        sender.pair(receiver)
        receiver.pair(sender)
        try:
            assert sender.data("status", "--json")["running"] is False
            receiver.data("start")
            time.sleep(3)  # Its initial announcement burst is over before sender starts.
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                starts = list(pool.map(lambda _: sender.data("start"), range(3)))
            pid = starts[0]["pid"]
            assert all(result["pid"] == pid for result in starts), starts
            assert (sender.config / "control.sock").stat().st_mode & 0o777 == 0o600
            idle_before = sender.data("status", "--json")["discovery"]
            observer.data("start")
            # Longer than the former 20-second refresh interval. Snapshot requests
            # and an idle daemon must initiate no probes or scans.
            end = time.monotonic() + 21
            while time.monotonic() < end:
                sender.rpc({"command": "devices"})
                time.sleep(0.4)
            idle_after = sender.data("status", "--json")["discovery"]
            assert idle_before["probe_rounds"] == idle_after["probe_rounds"] == 0
            assert idle_before["full_scans"] == idle_after["full_scans"] == 0
            assert idle_before["announcement_rounds"] == idle_after["announcement_rounds"] == 1
            neighbor = next(d for d in sender.rpc({"command": "devices"})["devices"] if d["fingerprint"] == observer.fingerprint)
            assert neighbor["paired"] is False and neighbor["online"] is False
            sender.rpc({"command": "menu_open", "session": "integration-menu"})
            eventually(lambda: any(d["fingerprint"] == observer.fingerprint and d["online"] for d in sender.menu()["devices"]))
            devices = eventually(lambda: [d for d in sender.menu()["devices"]
                                         if d["fingerprint"] == receiver.fingerprint and d["online"]])
            assert len(devices) == 1
            assert sender.rpc({"command": "send", "target": receiver.fingerprint,
                               "paths": [str(base / "missing")], "text": None})["ok"] is False
            assert sender.data("status", "--json")["pid"] == pid

            folder = base / "中文 空格目录"
            nested = folder / "第二层"
            nested.mkdir(parents=True)
            files = {
                folder / "linux.wps": b"synthetic WPS-like test bytes\x00\xff",
                nested / "line\n$(touch SHOULD_NOT_EXIST).txt": "多层目录内容".encode(),
            }
            for path, content in files.items():
                path.write_bytes(content)
            job = sender.data("send", "--to", receiver.fingerprint, "--", str(folder))["job_id"]

            def completed():
                for result in sender.data("status", "--json")["recent_jobs"]:
                    if result["id"] == job:
                        assert result["success"], (result, sender.log(), receiver.log())
                        return result

            eventually(completed)
            for path, content in files.items():
                # Upstream's CLI receiver intentionally flattens peer-supplied paths
                # and replaces control characters. Sender-side relative names are
                # covered by the existing directory metadata unit test.
                received = receiver.destination / path.name.replace("\n", "_")
                assert received.read_bytes() == content, received
            assert sender.data("status", "--json")["pid"] == pid
            assert receiver.data("status", "--json")["pending_sender"] is None
            assert not (base / "SHOULD_NOT_EXIST").exists()

            # New work reuses the same process, rather than taking the port again.
            extra = base / "再次发送.txt"
            extra.write_text("第二次传输")
            second = sender.data("send", "--to", receiver.name, str(extra))["job_id"]
            eventually(lambda: any(j["id"] == second and j["success"]
                                   for j in sender.data("status", "--json")["recent_jobs"]))
            assert (receiver.destination / extra.name).read_text() == "第二次传输"
            assert sender.data("status", "--json")["pid"] == pid

            # This menu keeps an already-confirmed result even if the peer stops.
            # Poll beyond the former six-second expiry without re-opening it.
            receiver.stop()
            held_rounds = sender.data("status", "--json")["discovery"]["probe_rounds"]
            end = time.monotonic() + 7
            while time.monotonic() < end:
                assert any(d["fingerprint"] == receiver.fingerprint and d["online"]
                           for d in sender.menu()["devices"])
                time.sleep(0.2)
            assert sender.data("status", "--json")["discovery"]["probe_rounds"] == held_rounds
            # Re-opening explicitly reconfirms and detects that it is now offline.
            sender.rpc({"command": "menu_open", "session": "integration-menu"})
            eventually(lambda: any(d["fingerprint"] == receiver.fingerprint and not d["online"]
                                   for d in sender.menu()["devices"]), timeout=7)
            cadence_start = sender.data("status", "--json")["discovery"]["probe_rounds"]
            end = time.monotonic() + 11
            while time.monotonic() < end:
                sender.menu()
                time.sleep(0.2)
            cadence_end = sender.data("status", "--json")["discovery"]["probe_rounds"]
            candidate_count = max(1, len(sender.rpc({"command": "devices"})["devices"]))
            assert 1 <= cadence_end - cadence_start <= 12 * candidate_count, (cadence_start, cadence_end, candidate_count)
            sender.rpc({"command": "menu_close", "session": "integration-menu"})
            closed = sender.data("status", "--json")["discovery"]
            time.sleep(6)
            assert sender.data("status", "--json")["discovery"] == closed
            # A lost GUI close event is also bounded by the lease.
            sender.rpc({"command": "menu_open", "session": "lost-menu"})
            time.sleep(3)
            expired = sender.data("status", "--json")["discovery"]
            assert expired["menu_sessions"] == 0
            time.sleep(5)
            assert sender.data("status", "--json")["discovery"] == expired
            sender.rpc({"command": "menu_open", "session": "integration-menu"})
            impostor = Peer(base, "impostor")
            impostor.port = receiver.port
            (impostor.config / "config.toml").write_text(
                f"alias = {json.dumps(receiver.name)}\nport = {impostor.port}\n"
                f"destination = {json.dumps(str(impostor.destination))}\n"
            )
            try:
                impostor.data("start")
                probes_before = sender.data("status", "--json")["discovery"]["probe_rounds"]
                def reprobed():
                    sender.menu()
                    return sender.data("status", "--json")["discovery"]["probe_rounds"] > probes_before
                eventually(reprobed, timeout=7)
                eventually(lambda: any(d["fingerprint"] == receiver.fingerprint and not d.get("checking") and not d["online"]
                                       for d in sender.menu()["devices"]), timeout=7)
            finally:
                impostor.stop()
            receiver.data("start")
            eventually(lambda: any(d["fingerprint"] == receiver.fingerprint and d["online"]
                                   for d in sender.menu()["devices"]), timeout=10)

            sender.rpc({"command": "menu_close", "session": "integration-menu"})
            receiver.stop()
            observer.stop()
            # Explicit refresh is bounded even if known peers are offline.
            start = time.monotonic()
            sender.rpc({"command": "refresh", "timeout_ms": 4000})
            eventually(lambda: not sender.data("status", "--json")["discovery"]["searching"], timeout=4.6)
            elapsed = time.monotonic() - start
            assert elapsed <= 4.6, elapsed
            # Force a timeout before the one-second subnet grace expires. This
            # verifies the discovery future is actually canceled, not just hidden.
            started = time.monotonic()
            sender.rpc({"command": "refresh", "timeout_ms": 100})
            eventually(lambda: not sender.data("status", "--json")["discovery"]["searching"], timeout=0.6)
            assert time.monotonic() - started < 0.6
            print("Policy timings:", json.dumps({"idle_seconds":21,"probe_rounds_in_11_seconds":cadence_end-cadence_start,"manual_refresh_seconds":round(elapsed,3)}))
            sender.data("send", "--to", "nonexistent-test-device", str(extra))
            assert sender.run("stop", check=False).returncode != 0
            sender.stop()
            assert sender.data("status", "--json")["running"] is False
            assert not (sender.config / "control.sock").exists()
            receiver.stop()
            print("PASS: passive idle discovery, read-only snapshots, bounded fast retries for offline peers,")
            print("      confirmed peers stop probing and do not expire; reopening reconfirms,")
            print("      menu close and lost-menu expiry, four-second cancellation, on-demand start, concurrent singleton, device submenu backend, same-user socket,")
            print("      real HTTPS folder transfer, nested Unicode/newline paths, paired auto-accept,")
            print("      repeated sends without exit, active online/offline checks, TLS identity mismatch rejection,")
            print("      queue protection, manual stop and cleanup.")
        finally:
            sender.stop()
            receiver.stop()
            observer.stop()
