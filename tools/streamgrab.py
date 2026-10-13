"""Takes a few seconds of one preview surface off the socket, as it is sent.

The surface is H.264 in Annex-B, sixteen bytes of header per frame, over a
websocket -- so this speaks just enough websocket to collect it and writes the
payload out for a decoder to look at. A watcher on the machine itself needs no
ticket (`stream_routes::watch`: `if !local && !authenticated`), which is why
this runs here.

    python3 -I tools/streamgrab.py composition-masked /var/tmp/masked.h264 4

and then, on a machine that has one:

    ffmpeg -f h264 -i masked.h264 -frames:v 1 -update 1 masked.png

ASCII only on purpose: this is read through an ssh round trip.
"""

import base64
import os
import socket
import struct
import sys
import time

HOST, PORT = "127.0.0.1", 80


def handshake(connection, path):
    key = base64.b64encode(os.urandom(16)).decode()
    request = (
        "GET %s HTTP/1.1\r\n"
        "Host: %s\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        "Sec-WebSocket-Key: %s\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    ) % (path, HOST, key)
    connection.sendall(request.encode())

    answer = b""
    while b"\r\n\r\n" not in answer:
        piece = connection.recv(4096)
        if not piece:
            raise SystemExit("the server closed before answering the upgrade")
        answer += piece
    head, _, rest = answer.partition(b"\r\n\r\n")
    first = head.split(b"\r\n")[0].decode("ascii", "replace")
    if "101" not in first:
        raise SystemExit("no upgrade: %s" % first)
    return rest


class Frames:
    """Websocket frames off a socket, one message at a time."""

    def __init__(self, connection, leftover=b""):
        self.connection = connection
        self.buffer = bytearray(leftover)

    def take(self, count):
        while len(self.buffer) < count:
            piece = self.connection.recv(1 << 16)
            if not piece:
                raise EOFError("the socket closed")
            self.buffer += piece
        out = bytes(self.buffer[:count])
        del self.buffer[:count]
        return out

    def message(self):
        """Returns (opcode, payload). Fragments are joined."""
        opcode, payload = None, b""
        while True:
            first, second = self.take(2)
            final = bool(first & 0x80)
            this_opcode = first & 0x0F
            masked = bool(second & 0x80)
            length = second & 0x7F
            if length == 126:
                length = struct.unpack(">H", self.take(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self.take(8))[0]
            mask = self.take(4) if masked else None
            body = self.take(length)
            if mask:
                body = bytes(byte ^ mask[i % 4] for i, byte in enumerate(body))
            if this_opcode != 0:
                opcode = this_opcode
            payload += body
            if final:
                return opcode, payload

    def pong(self, payload):
        head = bytes([0x8A, len(payload) | 0x80]) + b"\x00\x00\x00\x00"
        self.connection.sendall(head + payload)


def grab(surface, destination, seconds):
    connection = socket.create_connection((HOST, PORT), timeout=20)
    leftover = handshake(connection, "/api/v1/preview/stream/" + surface)
    frames = Frames(connection, leftover)

    picture = bytearray()
    started, pictures, said, first_header = time.time(), 0, [], None
    while time.time() - started < seconds:
        try:
            opcode, payload = frames.message()
        except EOFError:
            break
        if opcode == 0x8:
            said.append("closed: %s" % payload[2:].decode("utf-8", "replace"))
            break
        if opcode == 0x9:
            frames.pong(payload)
            continue
        if opcode == 0x1:
            said.append(payload.decode("utf-8", "replace"))
            continue
        if opcode != 0x2 or len(payload) < 16:
            continue
        version, flags = payload[0], payload[1]
        width, height = struct.unpack("<HH", payload[4:8])
        if first_header is None:
            if not flags & 1:
                # Before the first frame a watcher can start from there is no
                # sequence header, and a decoder has nothing to begin with.
                continue
            first_header = (version, flags, width, height)
        picture += payload[16:]
        pictures += 1

    connection.close()
    with open(destination, "wb") as out:
        out.write(picture)
    print("%-20s %d pictures, %d bytes, header=%s" % (surface, pictures, len(picture), first_header))
    for one in said:
        print("   says:", one)
    return pictures


if __name__ == "__main__":
    surface = sys.argv[1]
    destination = sys.argv[2]
    seconds = float(sys.argv[3]) if len(sys.argv) > 3 else 4.0
    sys.exit(0 if grab(surface, destination, seconds) else 1)
