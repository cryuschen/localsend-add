# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Verify the explicit terminal device command returns completed probe results.

Usage: python3 support/nautilus/check_terminal_devices.py target/release/localsend-cli
"""
import tempfile
from pathlib import Path
from check_background import Peer

with tempfile.TemporaryDirectory(prefix="ls-terminal-devices-") as directory:
    base = Path(directory)
    sender, receiver = Peer(base, "terminal-sender"), Peer(base, "terminal-receiver")
    sender.pair(receiver)
    receiver.pair(sender)
    try:
        receiver.data("start")
        sender.data("start")
        result = sender.data("devices", "--json")
        peer = next(d for d in result["devices"] if d["fingerprint"] == receiver.fingerprint)
        assert peer["online"] and not peer["checking"], peer
        state = sender.data("status", "--json")["discovery"]
        assert state["menu_sessions"] == 0 and state["full_scans"] == 1, state
        print("PASS: explicit terminal devices waits for confirmation; no menu lease or periodic scan.")
    finally:
        sender.stop()
        receiver.stop()
