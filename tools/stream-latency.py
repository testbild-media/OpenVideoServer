#!/usr/bin/env python3
"""How long it takes for a change on the composition to reach a watcher.

    python3 tools/stream-latency.py [composition|<output id>]

Run it on the server. It opens the preview stream, flips the composition from
one flat colour to another through the API, and reports how long it was before
the first picture that could carry the change came down the socket.

# Why this and not a stopwatch

The preview is what an operator cues from. "As low as possible" is the
requirement; a number is what makes it checkable. Everything else about the
stream has been measured — sixty pictures a second, six megabits, one encoder
however many are watching — and this is the one that decides whether somebody
can land a clip on a beat by eye.

# What is measured, and what is not

From the instant the API call returns to the instant the first picture arrives
that was encoded after it. That covers the render pass, the readback, the
encoder, the hop to the core and the socket. It does not cover a decoder, a
browser's frame queue or a screen's own latency, because those belong to the
watcher and differ between them.

A picture is recognised as carrying the change by being a keyframe or by being
much larger than the ones before it: a cut from one flat colour to another is
the largest thing a preview encoder ever sees. That is the honest limit of a
measurement made without decoding, and it is stated rather than hidden.

No third-party module: a WebSocket client is a handshake and a two-byte header,
and a tool that needed something installing would not be run.
"""

import base64
import os
import socket
import struct
import subprocess
import sys
import threading
import time

API = os.environ.get("OVS_BASE", "http://127.0.0.1:7420/api/v1")
HOST, PORT = "127.0.0.1", 7420

# How many times to ask. One sample is an anecdote: the answer depends on where
# in a pass the change landed, which is uniform over a frame.
ROUNDS = 8


def handshake(sock: socket.socket, path: str) -> None:
    """Asks for the upgrade, and reads the answer's headers."""
    key = base64.b64encode(os.urandom(16)).decode()
    request = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {HOST}:{PORT}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    sock.sendall(request.encode())

    buffer = b""
    while b"\r\n\r\n" not in buffer:
        piece = sock.recv(4096)
        if not piece:
            raise RuntimeError("the server closed before answering")
        buffer += piece

    head, rest = buffer.split(b"\r\n\r\n", 1)
    if b"101" not in head.split(b"\r\n")[0]:
        raise RuntimeError(head.decode(errors="replace").splitlines()[0])
    return rest


def frames(sock: socket.socket, rest: bytes, until: float):
    """Yields (arrival, payload) for every binary message, until the deadline."""
    buffer = rest
    sock.settimeout(0.5)

    def need(count: int) -> bool:
        nonlocal buffer
        while len(buffer) < count:
            if time.monotonic() > until:
                return False
            try:
                piece = sock.recv(65536)
            except socket.timeout:
                continue
            if not piece:
                return False
            buffer += piece
        return True

    while time.monotonic() < until:
        if not need(2):
            return
        first, second = buffer[0], buffer[1]
        length = second & 0x7F
        offset = 2

        if length == 126:
            if not need(4):
                return
            length = struct.unpack_from(">H", buffer, 2)[0]
            offset = 4
        elif length == 127:
            if not need(10):
                return
            length = struct.unpack_from(">Q", buffer, 2)[0]
            offset = 10

        # A server never masks, so there is no mask to strip.
        if not need(offset + length):
            return
        payload = buffer[offset : offset + length]
        buffer = buffer[offset + length :]

        if first & 0x0F == 0x02:  # binary
            yield time.monotonic(), payload
        elif first & 0x0F == 0x08:  # close
            return


def api(method: str, path: str, body: str | None = None) -> str:
    command = ["curl", "-s", "-X", method, f"{API}{path}"]
    if body is not None:
        command += ["-H", "content-type: application/json", "--data", body]
    return subprocess.run(command, capture_output=True, text=True, check=False).stdout


def main() -> int:
    surface = sys.argv[1] if len(sys.argv) > 1 else "composition"

    caught: list[tuple[float, bool, int]] = []
    ready = threading.Event()
    stop = time.monotonic() + 10.0 + ROUNDS * 3.0

    def watch() -> None:
        sock = socket.create_connection((HOST, PORT), timeout=5)
        rest = handshake(sock, f"/api/v1/preview/stream/{surface}")
        ready.set()
        for arrival, payload in frames(sock, rest, stop):
            if len(payload) < 16:
                continue
            keyframe = bool(payload[1] & 1)
            caught.append((arrival, keyframe, len(payload) - 16))
        sock.close()

    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    if not ready.wait(5.0):
        print("could not open the stream")
        return 1

    # Let it settle, so the first keyframe of the subscription is not mistaken
    # for the change.
    time.sleep(4.0)

    latencies: list[float] = []
    for attempt in range(ROUNDS):
        settled = len(caught)
        recent = sorted(size for _, _, size in caught[-60:]) or [0]
        middling = recent[len(recent) // 2]

        # The crossfader, because it is the one control that changes every pixel
        # of the composition at once and needs no project edit to do it.
        api("PUT", "/live/master", f'{{"crossfader":{1.0 if attempt % 2 == 0 else 0.0}}}')
        changed = time.monotonic()

        time.sleep(1.5)
        big = [
            (arrival, keyframe, size)
            for arrival, keyframe, size in caught[settled:]
            if arrival > changed and (keyframe or size > max(middling * 3, 2000))
        ]
        if big:
            arrival, _keyframe, size = big[0]
            latencies.append((arrival - changed) * 1000)
            print(f"  round {attempt + 1}: {latencies[-1]:5.0f} ms  ({size} bytes, typical {middling})")
        else:
            print(f"  round {attempt + 1}: nothing that could carry it")
        time.sleep(1.0)

    api("PUT", "/live/master", '{"crossfader":0.0}')

    if not latencies:
        print("nothing that could carry a change arrived at all")
        return 1

    latencies.sort()
    print(
        f"{len(latencies)} of {ROUNDS} rounds: "
        f"best {latencies[0]:.0f} ms, middle {latencies[len(latencies) // 2]:.0f} ms, "
        f"worst {latencies[-1]:.0f} ms"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
