#!/usr/bin/env python3
"""Phase 0 probe: reach the OpenDisplay receiver's TCP port through usbmuxd.

Read-only. Lists the devices usbmuxd knows, asks it to Connect to the given
port on the first USB device, then reads the receiver's first length-prefixed
message (its `hello`) and prints it. It sends nothing to the receiver; the
protocol drops a newcomer that closes without sending bytes (PROTOCOL.md
section 1), so a live session would not be disturbed.

    spikes/usbmux_probe.py [port]      # default 9000
"""
import json
import plistlib
import socket
import struct
import sys
import time

SOCKET_PATH = "/var/run/usbmuxd"
PLIST_MESSAGE = 8  # usbmuxd message type for XML plist payloads

# usbmuxd Result numbers, with what they mean for the user.
RESULTS = {
    0: "ok",
    2: "device not found (unplugged, or not paired: unlock it and tap Trust)",
    3: "connection refused (nothing listens on that port: open the OpenDisplay app)",
    6: "bad protocol version",
}


def request(sock, tag, body):
    payload = plistlib.dumps(
        {"ClientVersionString": "padlink-probe", "ProgName": "padlink-probe", **body}
    )
    sock.sendall(struct.pack("<IIII", 16 + len(payload), 1, PLIST_MESSAGE, tag) + payload)
    return read_reply(sock)


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError(f"connection closed after {len(buf)} of {n} bytes")
        buf += chunk
    return buf


def read_reply(sock):
    length, _version, _kind, _tag = struct.unpack("<IIII", read_exact(sock, 16))
    return plistlib.loads(read_exact(sock, length - 16))


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9000

    with socket.socket(socket.AF_UNIX) as s:
        s.connect(SOCKET_PATH)
        devices = request(s, 1, {"MessageType": "ListDevices"}).get("DeviceList", [])
    usb = [d for d in devices if d["Properties"].get("ConnectionType") == "USB"]
    for d in devices:
        p = d["Properties"]
        print(
            f"device {d['DeviceID']}: {p.get('ConnectionType')} "
            f"serial …{p.get('SerialNumber', '')[-6:]} speed {p.get('ConnectionSpeed')}"
        )
    if not usb:
        sys.exit("no USB device listed by usbmuxd: plug the iPad in and unlock it")

    s = socket.socket(socket.AF_UNIX)
    s.connect(SOCKET_PATH)
    t0 = time.monotonic()
    reply = request(
        s,
        2,
        {
            "MessageType": "Connect",
            "DeviceID": usb[0]["DeviceID"],
            # usbmuxd wants the port in network byte order inside a host integer.
            "PortNumber": socket.htons(port),
        },
    )
    number = reply.get("Number")
    print(f"Connect to port {port}: {RESULTS.get(number, f'result {number}')}")
    if number != 0:
        sys.exit(1)

    # From here the socket is a transparent pipe to the receiver.
    s.settimeout(5)
    try:
        (length,) = struct.unpack(">I", read_exact(s, 4))
        payload = read_exact(s, length)
    except (TimeoutError, EOFError) as e:
        sys.exit(f"tunnel opened but no hello arrived: {e}")
    print(f"first message after {1000 * (time.monotonic() - t0):.0f} ms, {length} bytes:")
    try:
        print(json.dumps(json.loads(payload), indent=2))
    except ValueError:
        print(payload[:200])
    s.close()


if __name__ == "__main__":
    main()
