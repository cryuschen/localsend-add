#!/usr/bin/env python3
# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Real loopback TLS regression using temporary device identities and files."""
import argparse
import http.client
import importlib.util
import json
from pathlib import Path
import ssl
import tempfile
import time
from urllib.parse import urlencode


def run(binary, check_idle=False):
    # Reuse the established isolated-peer fixture; the file only runs its
    # integration suite under __main__. It reads the binary from sys.argv[1].
    fixture = Path(__file__).parents[1] / "nautilus/check_background.py"
    spec = importlib.util.spec_from_file_location("desktop_receive_fixture", fixture)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.binary = str(binary.resolve())
    with tempfile.TemporaryDirectory(prefix="localsend-receive-regression-") as temporary:
        root = Path(temporary)
        sender, receiver = module.Peer(root, "test-sender"), module.Peer(root, "test-receiver")
        receiver.pair(sender)
        context = ssl._create_unverified_context()  # Only the generated loopback test server.
        context.load_cert_chain(str(sender.config / "identity.pem"))

        def connection():
            return http.client.HTTPSConnection("127.0.0.1", receiver.port, context=context, timeout=8)

        def post(endpoint, payload):
            client = connection()
            try:
                data = json.dumps(payload).encode() if isinstance(payload, dict) else payload
                client.request("POST", "/api/localsend/v2/" + endpoint, data)
                response = client.getresponse()
                return response.status, response.read()
            finally:
                client.close()

        info = {"alias": sender.name, "version": "2.2", "fingerprint": sender.fingerprint,
                "port": sender.port, "protocol": "https", "deviceType": "desktop"}

        def prepare(files):
            return post("prepare-upload", {"info": info, "files": {f["id"]: f for f in files}})

        def file(identifier, name, checksum=None):
            return {"id": identifier, "fileName": name, "size": 4,
                    "fileType": "application/octet-stream", "sha256": checksum}

        def upload_path(session, identifier):
            return "upload?" + urlencode({"sessionId": session["sessionId"], "fileId": identifier,
                                           "token": session["files"][identifier]})

        def wait_for(predicate, timeout=5, interval=0.03):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if predicate():
                    return
                time.sleep(interval)
            raise AssertionError("Timed out waiting for receive state")

        open_uploads = []
        try:
            receiver.data("start")
            code, body = prepare([file("checksum", "bad.bin", "0" * 64)])
            assert code == 200, (code, body)
            session = json.loads(body)
            code, _ = post(upload_path(session, "checksum"), b"BAD!")
            assert code == 422, code
            assert not list(receiver.destination.iterdir()), "checksum failure retained data"
            post("cancel?" + urlencode({"sessionId": session["sessionId"]}), b"")

            code, body = prepare([file("first", "same.txt"), file("second", "same.txt")])
            assert code == 200, (code, body)
            session = json.loads(body)
            # Both uploads reserve a destination while only temporary files
            # exist. A disk-existence-only name picker would choose the same name.
            for count, identifier in enumerate(("first", "second"), 1):
                client = connection()
                client.putrequest("POST", "/api/localsend/v2/" + upload_path(session, identifier))
                client.putheader("Content-Length", "4")
                client.endheaders()
                open_uploads.append(client)
                wait_for(lambda: len(list(receiver.destination.glob(".localsend-*.part"))) == count)
            assert not (receiver.destination / "same.txt").exists()
            open_uploads[0].send(b"AAAA")
            open_uploads[1].send(b"BBBB")
            for client in open_uploads:
                response = client.getresponse()
                assert response.status == 200, (response.status, response.read())
                response.read()
                client.close()
            open_uploads.clear()
            assert (receiver.destination / "same.txt").read_bytes() == b"AAAA"
            assert (receiver.destination / "same (1).txt").read_bytes() == b"BBBB"
            assert not list(receiver.destination.glob(".localsend-*.part"))
            print("PASS: real TLS checksum cleanup, concurrent same-name reservation and verified no-overwrite publication", flush=True)

            if check_idle:
                code, body = prepare([file("abandoned", "abandoned.bin")])
                assert code == 200, (code, body)
                started = time.monotonic()
                wait_for(lambda: prepare([file("next", "next.bin")])[0] == 200, timeout=130, interval=1.0)
                elapsed = time.monotonic() - started
                assert 115 <= elapsed <= 130, elapsed
                assert not (receiver.destination / "abandoned.bin").exists()
                print(f"PASS: abandoned TLS receive session released after {elapsed:.1f} seconds", flush=True)
        finally:
            for client in open_uploads:
                client.close()
            receiver.stop()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--idle", action="store_true", help="also observe the real two-minute idle deadline")
    args = parser.parse_args()
    run(args.binary, args.idle)
