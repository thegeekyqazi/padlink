#!/usr/bin/env python3
"""Phase 0, spike 5: count the control messages the receiver sends.

Listens on 127.0.0.1:<listen port>, forwards each connection through usbmuxd
to port 9000 on the iPad, and decodes the receiver-to-sender direction (every
frame there is a length-prefixed JSON control message). Bytes are passed on
unchanged in both directions.

    spikes/tap_proxy.py [listen port]       # default 9100
    testbench/od-sender.sh run --to 127.0.0.1:9100

Prints a count per message type every 5 s, and the first few `pencil` and
`proximity` messages in full. od-sender does not log those, which is why this
exists.
"""
import collections
import json
import plistlib
import socket
import struct
import sys
import threading
import time

USBMUXD = "/var/run/usbmuxd"
counts = collections.Counter()
samples = collections.Counter()
lock = threading.Lock()


def mux_request(sock, body):
    payload = plistlib.dumps({"ClientVersionString": "padlink-tap", "ProgName": "padlink-tap", **body})
    sock.sendall(struct.pack("<IIII", 16 + len(payload), 1, 8, 1) + payload)
    header = read_exact(sock, 16)
    return plistlib.loads(read_exact(sock, struct.unpack("<I", header[:4])[0] - 16))


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def dial_ipad():
    s = socket.socket(socket.AF_UNIX)
    s.connect(USBMUXD)
    devices = mux_request(s, {"MessageType": "ListDevices"})["DeviceList"]
    s.close()
    usb = [d for d in devices if d["Properties"].get("ConnectionType") == "USB"]
    if not usb:
        raise RuntimeError("no USB device listed by usbmuxd")
    s = socket.socket(socket.AF_UNIX)
    s.connect(USBMUXD)
    reply = mux_request(
        s, {"MessageType": "Connect", "DeviceID": usb[0]["DeviceID"], "PortNumber": socket.htons(9000)}
    )
    if reply.get("Number") != 0:
        raise RuntimeError(f"usbmuxd Connect failed with result {reply.get('Number')}: is the app open?")
    return s


def pump_raw(src, dst):
    try:
        while data := src.recv(65536):
            dst.sendall(data)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def pump_receiver(ipad, sender):
    """iPad to sender: forward, and decode each frame on the way."""
    try:
        while True:
            header = read_exact(ipad, 4)
            payload = read_exact(ipad, struct.unpack(">I", header)[0])
            sender.sendall(header + payload)
            try:
                kind = json.loads(payload).get("type", "?")
            except ValueError:
                kind = "<not json>"
            with lock:
                counts[kind] += 1
                if kind in ("pencil", "proximity") and samples[kind] < 5:
                    samples[kind] += 1
                    print(f"sample {kind}: {payload.decode(errors='replace')}", flush=True)
    except (EOFError, OSError):
        pass
    finally:
        for s in (ipad, sender):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def report():
    while True:
        time.sleep(5)
        with lock:
            line = " ".join(f"{k}={v}" for k, v in sorted(counts.items())) or "(nothing yet)"
        print(f"from iPad: {line}", flush=True)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9100
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", port))
    server.listen()
    print(f"listening on 127.0.0.1:{port}", flush=True)
    threading.Thread(target=report, daemon=True).start()
    while True:
        sender, _ = server.accept()
        sender.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        try:
            ipad = dial_ipad()
        except (RuntimeError, OSError) as e:
            print(f"cannot reach the iPad: {e}", flush=True)
            sender.close()
            continue
        print("sender connected, tunnel open", flush=True)
        threading.Thread(target=pump_raw, args=(sender, ipad), daemon=True).start()
        threading.Thread(target=pump_receiver, args=(ipad, sender), daemon=True).start()


if __name__ == "__main__":
    main()
