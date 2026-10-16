"""Everything that has to be true before a show, in one command.

    scp tools/preflight.py <server>:~/preflight.py
    ssh <server> python3 -I ~/preflight.py

It runs **on the server**, because the API trusts localhost and refuses
everybody else. Each line is OK or PROBLEM, and a PROBLEM says what to do
about it. It exits non-zero when anything needs looking at, so it can be the
last thing a get-in script does.

# Why these checks and not others

Every one of them is something that was found wrong on a machine that
otherwise looked healthy, mostly on the morning of 2026-10-16, going through
the show machine before its first event:

- **Art-Net and sACN were both switched off** in the database. The default is
  both on; something or somebody had turned them off, and nothing anywhere
  said so. A desk would have been plugged in, sending, and ignored.
- **A flag is not a picture.** `previews_enabled` said nothing useful on its
  own, so the preview is fetched twice and compared: a still preview means a
  black or frozen show, which is the thing you actually want to know.
- **The composition ran at 60.000 into an output that can only do 59.9401**,
  which costs a frame every couple of minutes for no reason at all.

So: nothing here is checked by reading a setting where a measurement was
possible, and the DMX check waits to see real packets rather than trusting
that the transport is enabled.
"""

import hashlib
import json
import sys
import time
import urllib.request

BASE = "http://127.0.0.1/api/v1"
TROUBLE = []


def fetched(path, raw=False):
    """Asks the server, which only answers its own machine."""
    with urllib.request.urlopen(BASE + path, timeout=15) as answer:
        body = answer.read()
    return body if raw else json.loads(body)


def says(ok, line, remedy=""):
    """One finding, and what to do about it when it is not good news."""
    print("  %-8s %s" % ("OK" if ok else "PROBLEM", line))
    if not ok:
        if remedy:
            print("           -> %s" % remedy)
        TROUBLE.append(line)


print("== the server ==")
try:
    status = fetched("/server/status")
except Exception as trouble:  # noqa: BLE001 - whatever went wrong, the answer is the same
    print("  PROBLEM  the server does not answer: %s" % trouble)
    print("           -> systemctl status ovsd")
    sys.exit(1)

telemetry = status["telemetry"]
says(True, "version %s, up %d min" % (telemetry["version"], telemetry["uptime_s"] // 60))
says(
    telemetry["renderer"]["state"] == "running",
    "renderer is %s" % telemetry["renderer"]["state"],
    "systemctl restart ovs-renderer",
)
says(
    bool(telemetry.get("current_show")),
    "show loaded: %s" % telemetry.get("current_show_name"),
    "open a show from the client",
)

print("== the desk ==")
protocols = status["dmx_protocols"]
says(
    protocols["artnet"] or protocols["sacn"],
    "listening: artnet=%s sacn=%s" % (protocols["artnet"], protocols["sacn"]),
    "both of these can be off in the database and nothing on screen says so",
)
dmx = telemetry["dmx"]
before = dmx["artnet_packets"] + dmx["sacn_packets"]
time.sleep(3.0)
dmx = fetched("/server/status")["telemetry"]["dmx"]
arriving = dmx["artnet_packets"] + dmx["sacn_packets"] - before
says(
    arriving > 0,
    "%d packets in three seconds, universes %s" % (arriving, dmx["universes"]),
    "nothing is sending. check the desk, the cable and the universe numbers",
)
says(
    dmx["unusable_packets"] == 0 and dmx["refused_packets"] == 0,
    "unusable %d, refused %d" % (dmx["unusable_packets"], dmx["refused_packets"]),
    "something is sending that this show will not take",
)

print("== the wall ==")
for output in telemetry["outputs"]:
    refresh = output["refresh"]
    wanted = refresh["numerator"] / refresh["denominator"]
    says(
        output["active"],
        "%s: %dx%d @ %.3f, actually %.3f"
        % (
            output["name"],
            output["size"]["width"],
            output["size"]["height"],
            wanted,
            output["actual_fps"],
        ),
        "the output is not active",
    )

body = {}
if telemetry.get("current_show"):
    show = fetched("/shows/" + telemetry["current_show"])
    body = show.get("show", show)
    rate = body["composition"]["render_rate"]
    composed = rate["numerator"] / rate["denominator"]
    for output in telemetry["outputs"]:
        refresh = output["refresh"]
        wanted = refresh["numerator"] / refresh["denominator"]
        says(
            abs(composed - wanted) < 0.001,
            "composition %.4f vs %s %.4f" % (composed, output["connector"], wanted),
            "a composition that does not match the output loses a frame now and then",
        )

# The panel in the hall is not the panel on the bench, and a driver builds no
# mode a panel did not advertise. A show carrying a rate this wall does not
# offer is a dark wall, and it looks exactly like a broken renderer.
print("== what this panel actually offers ==")
connectors = {
    (c["gpu_pci_address"], c["connector"]): c
    for c in fetched("/server/capabilities")["connectors"]
}
for connector in connectors.values():
    if not connector.get("connected"):
        continue
    sizes = {}
    for mode in connector.get("modes", []):
        key = "%dx%d" % (mode["size"]["width"], mode["size"]["height"])
        sizes.setdefault(key, []).append(mode["refresh_millihertz"] / 1000.0)
    print(
        "  %s on %s (%s)"
        % (connector.get("model") or "unnamed panel", connector["connector"], connector["role"])
    )
    for size in sorted(sizes):
        print("      %-12s %s" % (size, ", ".join("%.3f" % r for r in sorted(sizes[size]))))

for output in body.get("outputs", []) or []:
    identity = output.get("identity", {})
    connector = connectors.get((identity.get("gpu_pci_address"), identity.get("connector")))
    mode = output["mode"]
    asked = mode["refresh"]["numerator"] / mode["refresh"]["denominator"]
    wanted = "%dx%d @ %.3f" % (mode["size"]["width"], mode["size"]["height"], asked)
    if connector is None or not connector.get("connected"):
        says(
            False,
            "%s wants %s on %s, which has nothing plugged in"
            % (output["name"], wanted, identity.get("connector")),
            "plug the wall in, or point this output at the socket it is on",
        )
        continue
    offered = any(
        mode["size"]["width"] == offer["size"]["width"]
        and mode["size"]["height"] == offer["size"]["height"]
        and abs(offer["refresh_millihertz"] - asked * 1000.0) < 2.0
        for offer in connector.get("modes", [])
    )
    says(
        offered,
        "%s wants %s, and %s offers it" % (output["name"], wanted, connector["connector"]),
        "this panel does not build that mode -- pick one listed above, in the client's "
        "output settings, and set the composition to the same rate",
    )

print("== a real picture ==")
says(status["previews_enabled"], "previews are on", "the client will show no pictures")
shots = []
refused = ""
# Only worth asking when they are on: a refusal with previews off is the same
# fact reported twice, and two problems for one cause is how a list stops
# being read.
for index in range(2 if status["previews_enabled"] else 0):
    if index:
        time.sleep(1.5)
    try:
        shots.append(hashlib.sha256(fetched("/preview/composition", raw=True)).hexdigest())
    except Exception as trouble:  # noqa: BLE001 - a refusal is a finding, not a crash
        refused = str(trouble)
        break
if refused:
    # Never a traceback here. This runs on a get-in, under time pressure, and a
    # check that dies on the one thing it is meant to report is worse than none.
    says(
        False,
        "the composition preview did not come: %s" % refused,
        "with previews on, this means the renderer has not sent one -- "
        "check that it is running and that a show is loaded",
    )
elif len(shots) == 2:
    says(
        shots[0] != shots[1],
        "the composition preview is moving",
        "a still preview means a black or frozen show -- check the layers and the master dimmer",
    )

print("== room to run ==")
says(telemetry["decode_errors"] == 0, "decode errors: %d" % telemetry["decode_errors"])
free = telemetry["ram_total_bytes"] - telemetry["ram_used_bytes"]
says(free > 4 * 2**30, "memory free: %.1f GiB" % (free / 2**30))
says(
    telemetry["disk_used_bytes"] < 0.9 * telemetry["disk_total_bytes"],
    "disk: %.0f of %.0f GiB used"
    % (telemetry["disk_used_bytes"] / 2**30, telemetry["disk_total_bytes"] / 2**30),
)
says(
    telemetry["vram_used_bytes"] < 0.85 * telemetry["vram_total_bytes"],
    "vram: %.0f of %.0f MiB"
    % (telemetry["vram_used_bytes"] / 2**20, telemetry["vram_total_bytes"] / 2**20),
)
says(
    telemetry["cpu_temperature_c"] < 85 and telemetry["gpu_temperature_c"] < 85,
    "cpu %.0f C, gpu %.0f C" % (telemetry["cpu_temperature_c"], telemetry["gpu_temperature_c"]),
)

print()
if TROUBLE:
    print("%d thing(s) to look at before doors." % len(TROUBLE))
    sys.exit(1)
print("Nothing to look at. Ready.")
