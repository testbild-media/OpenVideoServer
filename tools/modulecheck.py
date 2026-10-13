"""Drives the running server through every generator and every effect.

Runs on the machine itself. It builds a show of its own, puts each module on a
layer in turn, pushes every parameter the module declares, and reads the
composition preview back as pixels -- so what is checked is what the card drew,
not what a unit test believed.

Nothing is assumed about which modules exist: the catalogue the server serves is
the list, so this reports on the build that is installed rather than on the one
the source tree has. Parameter keys, ranges and defaults come from the same
place, which is why a build that renames a parameter cannot quietly go
unchecked here.

    python3 -I tools/modulecheck.py            # every module the build serves
    python3 -I tools/modulecheck.py ovs.blur   # one of them

What it took five runs to learn, written down where it is easy to undo by
accident. Each of these reported something false with complete confidence
first:

* **Switch through black.** Reading the preview straight after selecting a slot
  reads the picture the previous module left behind. Waiting for "the picture
  changed" is no good either -- against an animated module it is true on the
  very next frame. Black is a state that can be recognised, so every change of
  module goes through the black slot and waits for it both ways.
* **Live values merge.** The state keeps the override map it has unless `reset`
  is set, so "this parameter on its own" is really "this parameter on top of
  whatever the last push left behind". Every push here carries reset.
* **Judge a moving module by numbers, not by frames.** Noise differs from
  itself every frame, so comparing one frame to another says every parameter
  works. A still module is compared pixel by pixel; a moving one by figures
  taken over several frames against its own wobble -- and those figures have to
  include tile means, or a caption change disappears into a whole-frame
  average.
* **Sample unevenly.** The preview is encoded at fifteen frames a second. A ten
  hertz strobe read ten times a second is read at the same point of every flash
  and comes back as a still picture.
* **Switch a parameter on, not merely away from its default.** Blur's amount
  defaults to full, so "as far from the default as possible" turns it off.
  Every parameter is tried twice: on its own over the defaults, and with
  everything on and this one put back. A circle's turn cannot be seen the first
  way and a frame-filling square's cannot be seen the second.
ASCII only on purpose: this is read through an ssh round trip.
"""

import hashlib
import json
import random
import struct
import sys
import time
import urllib.error
import urllib.request
import zlib

API = "http://127.0.0.1/api/v1"
CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

# How long to wait for a picture to arrive after a change, and how often to look.
SETTLE_TRIES = 25
SETTLE_PAUSE = 0.2
# A picture that differs from another by less than this, per channel on average,
# counts as the same picture. The preview is a scaled copy, so a change worth
# seeing is far larger than this.
SAME = 0.6
# How many of its own wobbles a moving module has to move before the move is
# taken to be the parameter rather than the animation.
WOBBLES = 3.0
# How long the renderer is given to draw what was just sent before the
# measurement starts. A frame at sixty hertz is sixteen milliseconds; this is
# the preview's own rate plus room for the state to travel.
PUSH_SETTLE = 0.45
# Frames taken to describe one setting.
FRAMES = 4
FRAME_PAUSE = 0.22
# The grid the local measurement uses.
TILES_ACROSS = 12
TILES_DOWN = 8
# Live values are MERGED into whatever is already there -- `set_effect_parameters`
# keeps the current map unless `reset` is set, and then inserts what arrived. So
# every push here carries reset: without it "this parameter on its own" is
# really "this parameter on top of everything the last push left behind", and
# every measurement after the first is of the same picture.
# Parameters that switch an effect on. Their enabling end is the top of their
# range, not the end farthest from the default.
MASTERS = ("amount", "master", "mix", "strength")


# --- talking to the server ---------------------------------------------------


def call(method, path, body=None, raw=False):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(API + path, data=data, method=method)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request, timeout=30) as answer:
        payload = answer.read()
    if raw:
        return payload
    if not payload:
        return None
    return json.loads(payload)


def refused(method, path, body=None):
    """Returns the server's complaint, or None when it accepted."""
    try:
        call(method, path, body)
        return None
    except urllib.error.HTTPError as error:
        return "%d %s" % (error.code, error.read().decode("utf-8", "replace")[:240])
    except Exception as error:  # noqa: BLE001 - reported, not handled
        return str(error)


def ulid():
    value = (int(time.time() * 1000) << 80) | random.getrandbits(80)
    return "".join(CROCKFORD[(value >> shift) & 31] for shift in range(125, -1, -5))


# --- reading a PNG without a library ----------------------------------------


def pixels_of(png):
    """Returns (width, height, rows) with rows as lists of (r, g, b, a)."""
    if png[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    at, width, height, depth, colour, data = 8, 0, 0, 0, 0, b""
    while at < len(png):
        length = struct.unpack(">I", png[at : at + 4])[0]
        kind = png[at + 4 : at + 8]
        if kind == b"IHDR":
            width, height, depth, colour = struct.unpack(">IIBB", png[at + 8 : at + 18])
        elif kind == b"IDAT":
            data += png[at + 8 : at + 8 + length]
        elif kind == b"IEND":
            break
        at += 12 + length
    if depth != 8 or colour not in (2, 6):
        raise ValueError("expected eight-bit RGB or RGBA, got depth %d colour %d" % (depth, colour))

    channels = 3 if colour == 2 else 4
    stride = width * channels
    flat = zlib.decompress(data)

    rows, previous = [], bytes(stride)
    at = 0
    for _ in range(height):
        filter_kind = flat[at]
        line = flat[at + 1 : at + 1 + stride]
        at += 1 + stride
        if filter_kind == 0:
            pass
        elif filter_kind == 2:
            # The one the encoder uses for nearly every row, so it is worth
            # doing in one pass rather than byte by byte.
            line = bytes((a + b) & 0xFF for a, b in zip(line, previous))
        else:
            work = bytearray(line)
            for i in range(stride):
                left = work[i - channels] if i >= channels else 0
                up = previous[i]
                corner = previous[i - channels] if i >= channels else 0
                if filter_kind == 1:
                    work[i] = (work[i] + left) & 0xFF
                elif filter_kind == 3:
                    work[i] = (work[i] + (left + up) // 2) & 0xFF
                elif filter_kind == 4:
                    guess = left + up - corner
                    da, db, dc = abs(guess - left), abs(guess - up), abs(guess - corner)
                    near = left if (da <= db and da <= dc) else (up if db <= dc else corner)
                    work[i] = (work[i] + near) & 0xFF
                else:
                    raise ValueError("unknown row filter %d" % filter_kind)
            line = bytes(work)
        rows.append(
            [
                (line[x], line[x + 1], line[x + 2], line[x + 3] if channels == 4 else 255)
                for x in range(0, stride, channels)
            ]
        )
        previous = line
    return width, height, rows


def png_of(width, height, pixel):
    """Builds an eight-bit RGBA PNG. The other direction of `pixels_of`."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixel(x, y))

    def chunk(kind, body):
        head = struct.pack(">I", len(body)) + kind
        return head + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
        + chunk(b"IEND", b"")
    )


def upload(name, png, width, height):
    """Puts a picture in the library and returns its identifier.

    Three requests, as the module documentation describes them: say what is
    coming, send it, have it verified. Nothing is trusted until the server has
    hashed what actually arrived, so the hash here has to be the real one.
    """
    digest = hashlib.sha256(png).hexdigest()
    answer = call(
        "POST",
        "/media/uploads",
        {
            "display_name": name,
            "original_filename": name + ".png",
            "sha256": digest,
            "byte_size": len(png),
            "kind": "image",
            "dimensions": {"width": width, "height": height},
            "has_alpha": True,
            "container": "png",
            "codec": "png",
        },
    )
    if answer.get("result") == "already_present":
        return answer["media_item_id"]

    upload_id = answer["upload_id"]
    request = urllib.request.Request(
        API + "/media/uploads/%s?offset=0" % upload_id, data=png, method="PUT"
    )
    request.add_header("Content-Type", "application/octet-stream")
    with urllib.request.urlopen(request, timeout=60) as sent:
        sent.read()
    item = call("POST", "/media/uploads/%s/verify" % upload_id, None)
    return item["id"]


# The picture the Mask effect is driven with, uploaded once per run. Black down
# the left half and white down the right, so what it does is a thing that can be
# counted rather than a thing that looks about right.
STENCIL = {"id": None, "width": 0, "height": 0}


class Picture:
    def __init__(self, png):
        self.width, self.height, self.rows = pixels_of(png)
        self.mark = zlib.crc32(repr(self.rows).encode()) & 0xFFFFFFFF

    def __repr__(self):
        return "%dx%d mean=%s" % (self.width, self.height, tuple(round(v) for v in self.mean()))

    def box(self, left=0.0, top=0.0, right=1.0, bottom=1.0):
        x0, x1 = int(self.width * left), max(int(self.width * right), int(self.width * left) + 1)
        y0, y1 = int(self.height * top), max(int(self.height * bottom), int(self.height * top) + 1)
        for row in self.rows[y0:y1]:
            for pixel in row[x0:x1]:
                yield pixel

    def mean(self, **where):
        total, count = [0, 0, 0, 0], 0
        for pixel in self.box(**where):
            for c in range(4):
                total[c] += pixel[c]
            count += 1
        return tuple(value / max(count, 1) for value in total)

    def flat(self):
        first = self.rows[0][0]
        return all(pixel == first for pixel in self.box())

    def greyness(self):
        """How far from grey the picture is, nought to 255."""
        total, count = 0, 0
        for pixel in self.box():
            total += max(pixel[:3]) - min(pixel[:3])
            count += 1
        return total / max(count, 1)

    def dark_fraction(self, **where):
        dark, count = 0, 0
        for pixel in self.box(**where):
            if sum(pixel[:3]) < 24:
                dark += 1
            count += 1
        return dark / max(count, 1)

    def edginess(self):
        """Mean step between neighbouring pixels, both ways.

        Both ways on purpose: a gradient straight down the picture has no
        side-to-side steps at all, and a measurement that only looked sideways
        would call it blank.
        """
        total, count = 0, 0
        for y, row in enumerate(self.rows):
            below = self.rows[y + 1] if y + 1 < self.height else None
            for x in range(len(row)):
                here = row[x]
                if x + 1 < len(row):
                    right = row[x + 1]
                    total += abs(here[0] - right[0]) + abs(here[1] - right[1])
                    count += 1
                if below is not None:
                    under = below[x]
                    total += abs(here[0] - under[0]) + abs(here[1] - under[1])
                    count += 1
        return total / max(count, 1)

    def uniform_blocks(self, size):
        """What share of the blocks of this size hold exactly one colour.

        What pixelation does, measured where it does it. Mean run length cannot
        see it: a picture that is mostly one flat colour already has runs far
        longer than any block.
        """
        whole, uniform = 0, 0
        for top in range(0, self.height - size + 1, size):
            for left in range(0, self.width - size + 1, size):
                first = self.rows[top][left]
                same = True
                for row in self.rows[top : top + size]:
                    for pixel in row[left : left + size]:
                        if pixel != first:
                            same = False
                            break
                    if not same:
                        break
                whole += 1
                uniform += 1 if same else 0
        return uniform / max(whole, 1)

    def colours(self):
        """How many different colours are in the picture."""
        return len({pixel for pixel in self.box()})

    def run_length(self):
        """Mean run of side-by-side pixels that are exactly equal. Rises with blocks."""
        runs, length = [], 1
        for row in self.rows:
            for x in range(1, len(row)):
                if row[x] == row[x - 1]:
                    length += 1
                else:
                    runs.append(length)
                    length = 1
            runs.append(length)
            length = 1
        return sum(runs) / max(len(runs), 1)

    def spread(self):
        """The widest difference between opposite edges of the picture.

        What tells a gradient from a flat fill whichever way round it runs.
        """
        across = abs(self.mean(right=0.12)[0] - self.mean(left=0.88)[0])
        down = abs(self.mean(bottom=0.12)[0] - self.mean(top=0.88)[0])
        return max(across, down)

    def folded(self):
        """How far the picture is from being its own mirror, side to side."""
        total, count = 0, 0
        for row in self.rows:
            for x in range(self.width // 2):
                other = row[self.width - 1 - x]
                here = row[x]
                for c in range(3):
                    total += abs(here[c] - other[c])
                    count += 1
        return total / max(count, 1)

    def tiles(self):
        """Mean brightness of each tile of a coarse grid over the picture.

        Whole-picture figures cannot see a caption change: the text is a small
        part of the frame. Tiles can, and a tile away from whatever a module
        animates has little wobble of its own to hide behind.
        """
        out = []
        high = self.height // TILES_DOWN
        wide = self.width // TILES_ACROSS
        for ty in range(TILES_DOWN):
            for tx in range(TILES_ACROSS):
                total = 0
                for row in self.rows[ty * high : (ty + 1) * high]:
                    for pixel in row[tx * wide : (tx + 1) * wide]:
                        total += pixel[0] + pixel[1] + pixel[2]
                out.append(total / max(high * wide * 3, 1))
        return out

    def figures(self):
        """Numbers that describe the picture whatever frame it is on."""
        mean = self.mean()
        return [
            mean[0],
            mean[1],
            mean[2],
            self.greyness(),
            self.edginess(),
            self.run_length(),
        ] + self.tiles()


def distance(one, other):
    """(mean, worst) difference per channel between two pictures of one size."""
    if (one.width, one.height) != (other.width, other.height):
        return (255.0, 255.0)
    total, worst, count = 0, 0, 0
    for a_row, b_row in zip(one.rows, other.rows):
        for a, b in zip(a_row, b_row):
            for c in range(3):
                step = abs(a[c] - b[c])
                total += step
                worst = max(worst, step)
                count += 1
    return (total / max(count, 1), float(worst))


def look():
    return Picture(call("GET", "/preview/composition", raw=True))


def samples(count=FRAMES, pause=FRAME_PAUSE):
    """Several looks, deliberately unevenly spaced.

    Evenly spaced is a trap. The preview is encoded at its own rate, and a
    strobe at ten hertz read ten times a second is read at the same point of
    every flash -- it comes back as a still picture. Uneven steps cannot land
    on the same phase twice.
    """
    taken = [look()]
    for index in range(count - 1):
        time.sleep(pause * (0.55 + 0.3 * (index % 4)))
        taken.append(look())
    return taken


def preview_rate(seconds=2.0):
    """How many different pictures a second the preview really hands out."""
    seen, began = [], time.time()
    while time.time() - began < seconds:
        seen.append(look().mark)
    changes = sum(1 for a, b in zip(seen, seen[1:]) if a != b)
    return changes / seconds, len(seen) / seconds


def wait_for(wanted, tries=SETTLE_TRIES):
    """Looks until the picture is what is wanted, or gives up and returns it."""
    seen = look()
    for _ in range(tries):
        if wanted(seen):
            return seen
        time.sleep(SETTLE_PAUSE)
        seen = look()
    return seen


def pushed(tries=4):
    """Gives the renderer a moment, then waits for the picture to come to rest.

    Waiting matters as much as the moment does: a still module measured while
    it is still changing is reported as a moving one, and then everything about
    it is judged by the measure meant for noise. A module that really does
    animate never comes to rest, so this gives up quickly.
    """
    time.sleep(PUSH_SETTLE)
    seen = look()
    for _ in range(tries):
        again = look()
        if again.mark == seen.mark:
            return
        time.sleep(SETTLE_PAUSE)
        seen = again


def is_black(picture):
    return sum(picture.mean()[:3]) < 2.0


class Setting:
    """What the composition looks like at one setting, over several frames."""

    def __init__(self, count=FRAMES, pause=FRAME_PAUSE):
        self.frames = samples(count, pause)
        rows = [one.figures() for one in self.frames]
        columns = list(zip(*rows))
        self.middle = [sum(column) / len(column) for column in columns]
        self.wobble = [
            max(abs(value - middle) for value in column)
            for column, middle in zip(columns, self.middle)
        ]
        self.movement = max(
            (distance(self.frames[0], other)[0] for other in self.frames[1:]), default=0.0
        )
        self.moving = self.movement > SAME

    @property
    def first(self):
        return self.frames[0]

    def __repr__(self):
        return repr(self.first)


def wobbles_apart(one, other):
    """How many of their own wobbles two settings are apart."""
    worst = 0.0
    for i in range(len(one.middle)):
        wobble = max(one.wobble[i], other.wobble[i], 0.05)
        worst = max(worst, abs(one.middle[i] - other.middle[i]) / wobble)
    return worst


def tell_apart(one, other):
    """Whether two settings are distinguishable, and the number that says so.

    A still picture is compared pixel by pixel, which is exact. A moving one is
    compared by its figures against its own wobble, because two frames of noise
    differ from each other more than any parameter would make them.
    """
    frame = distance(one.first, other.first)[0]
    if one.moving or other.moving:
        measure = wobbles_apart(one, other)
        return measure > WOBBLES, "wobbles=%.1f (distance=%.2f)" % (measure, frame)
    return frame > SAME, "distance=%.2f" % frame


# --- the report --------------------------------------------------------------


class Report:
    def __init__(self):
        self.lines = []
        self.passed = 0
        self.failed = 0
        self.warned = 0
        self.where = ""

    def section(self, title):
        print()
        print("=" * 74)
        print(title)
        print("=" * 74)
        sys.stdout.flush()

    def module(self, title):
        print()
        print("  " + title)
        self.where = title.split()[0]
        sys.stdout.flush()

    def check(self, label, ok, detail=""):
        if ok is None:
            self.warned += 1
            mark = "WARN"
        elif ok:
            self.passed += 1
            mark = "PASS"
        else:
            self.failed += 1
            mark = "FAIL"
        print("    %-4s %-32s %s" % (mark, label, detail))
        sys.stdout.flush()
        self.lines.append((mark, self.where, label, detail))

    def summary(self):
        print()
        print("=" * 74)
        print(
            "passed %d   failed %d   to look at %d"
            % (self.passed, self.failed, self.warned)
        )
        for wanted in ("FAIL", "WARN"):
            shown = [one for one in self.lines if one[0] == wanted]
            if not shown:
                continue
            print()
            for mark, where, label, detail in shown:
                print("  %-4s %-20s %-30s %s" % (mark, where, label, detail))
        print("=" * 74)


# --- parameter values -------------------------------------------------------


def default_value(parameter):
    kind = parameter["parameter_type"]["kind"]
    body = parameter["parameter_type"]
    if kind == "image":
        # No picture, which is what this kind starts at and the only default it
        # has: a published default may not name a row somebody can delete.
        return {"type": "text", "value": ""}
    tag = {
        "number": "float",
        "toggle": "bool",
        "colour": "color",
        "text": "text",
        "choice": "choice",
    }[kind]
    return {"type": tag, "value": body["default"]}


def enabling_value(parameter):
    """The value that switches this parameter on as far as it goes."""
    kind = parameter["parameter_type"]["kind"]
    body = parameter["parameter_type"]
    if kind == "number":
        default, low, high = body["default"], body["min"], body["max"]
        if parameter["key"] in MASTERS:
            # A master is switched on at the top of its range. Taking it to
            # whichever end is farther from the default would switch it off
            # whenever it defaults to full, which is how blur defaults.
            far = high
        elif parameter["key"].endswith("_deg") and high - low >= 360.0:
            # An angle. Its far end is the way back to where it started, so a
            # quarter turn is the push, and an eighth turn is where it is put
            # back to -- not the default, because a square turned by a quarter
            # is the square it started as.
            far = angle(default, low, high, 4.0)
        else:
            far = high if (high - default) >= (default - low) else low
        # A range that reaches into the thousands of pixels means "the whole
        # frame"; going all the way there usually leaves nothing to look at.
        if high > 600 and far == high:
            far = max(480.0, default + 1.0)
        if abs(far - default) < 1e-9:
            return None
        return {"type": "float", "value": far}
    if kind == "toggle":
        return {"type": "bool", "value": not body["default"]}
    if kind == "colour":
        # Away from wherever the default sits, so a colour that is already red
        # is not "changed" to itself.
        default = body["default"]
        red = [1.0, 0.0, 0.0, 1.0]
        cyan = [0.0, 1.0, 1.0, 1.0]
        far = cyan if sum(abs(a - b) for a, b in zip(default, red)) < 0.75 else red
        return {"type": "color", "value": far}
    if kind == "text":
        return {"type": "text", "value": "OVS 12:34"}
    if kind == "image":
        # The stencil this run uploaded. Without one there is nothing to switch
        # a picture parameter on with, and the sweep leaves it alone.
        if not STENCIL["id"]:
            return None
        return {"type": "text", "value": STENCIL["id"]}
    if kind == "choice":
        others = [one["id"] for one in body["choices"] if one["id"] != body["default"]]
        if not others:
            return None
        return {"type": "choice", "value": others[0]}
    raise ValueError("unknown parameter kind %s" % kind)


def angle(default, low, high, part):
    """A turn of one `part` of the way round, kept inside the declared range."""
    span = high - low
    return low + (default - low + span / part) % span


def opposite_value(parameter, enabling):
    """Where to put this parameter to switch it back off again.

    Its default, except where the default is the enabling end itself -- then
    the other end of the range.
    """
    kind = parameter["parameter_type"]["kind"]
    body = parameter["parameter_type"]
    if kind != "number":
        return default_value(parameter)
    default, low, high = body["default"], body["min"], body["max"]
    if parameter["key"].endswith("_deg") and high - low >= 360.0:
        return {"type": "float", "value": angle(default, low, high, 8.0)}
    if abs(default - enabling["value"]) > 1e-9:
        return {"type": "float", "value": default}
    other = low if abs(low - default) > abs(high - default) else high
    if abs(other - default) < 1e-9:
        return None
    return {"type": "float", "value": other}


def defaults_of(manifest):
    return {one["key"]: default_value(one) for one in manifest["parameters"]}


def colours_of(manifest, colour):
    return {
        one["key"]: {"type": "color", "value": colour}
        for one in manifest["parameters"]
        if one["parameter_type"]["kind"] == "colour"
    }


# --- what each module is expected to do -------------------------------------

# Modules that must animate with nothing driving them.
FREE_RUNNING = {
    ("ovs.clock", 1),
    ("ovs.countdown", 1),
    ("ovs.noise", 1),
    ("ovs.test_pattern", 2),
}

# Modules whose picture must stand still until something changes it.
STANDS_STILL = {
    ("ovs.solid_colour", 1),
    ("ovs.test_pattern", 1),
    ("ovs.gradient", 1),
    ("ovs.grid", 1),
    ("ovs.shape", 1),
    ("ovs.text", 1),
}


def generator_shape(report, type_id, picture):
    """Checks that only this generator can be checked for."""
    mean = picture.mean()
    if type_id == "ovs.solid_colour":
        report.check("one colour throughout", picture.flat(), repr(picture))
        report.check(
            "white at its defaults",
            all(abs(value - 255) <= 2 for value in mean[:3]),
            "mean=%s" % (tuple(round(v) for v in mean),),
        )
    elif type_id == "ovs.gradient":
        report.check(
            "runs across the picture",
            picture.spread() > 20,
            "edge to edge=%.0f" % picture.spread(),
        )
    elif type_id == "ovs.grid":
        report.check(
            "draws lines",
            not picture.flat() and picture.edginess() > 1.0,
            "edges=%.2f" % picture.edginess(),
        )
    elif type_id in ("ovs.shape", "ovs.text", "ovs.test_pattern", "ovs.countdown", "ovs.clock"):
        report.check("draws a figure", not picture.flat(), repr(picture))


def specific(width, height):
    """Checks that know what a particular effect is supposed to do."""
    half = round(width / 2.0)
    return {
        "ovs.colour_correct": [
            (
                "saturation nought is grey",
                {"saturation": {"type": "float", "value": 0.0}},
                lambda base, seen: (seen.greyness() < 2.0, "greyness=%.2f" % seen.greyness()),
            ),
            (
                "brightness lifts it",
                {"brightness": {"type": "float", "value": 2.0}},
                lambda base, seen: (
                    seen.mean()[0] > base.mean()[0] + 8,
                    "mean r %.0f -> %.0f" % (base.mean()[0], seen.mean()[0]),
                ),
            ),
        ],
        "ovs.rgb_correct": [
            (
                "red all the way down",
                {"gain_r": {"type": "float", "value": 0.0}},
                lambda base, seen: (
                    seen.mean()[0] < 3.0,
                    "mean r %.0f -> %.2f" % (base.mean()[0], seen.mean()[0]),
                ),
            ),
        ],
        "ovs.colorize": [
            (
                "tinted green",
                {
                    "amount": {"type": "float", "value": 1.0},
                    "saturation": {"type": "float", "value": 1.0},
                    "hue": {"type": "float", "value": 120.0},
                },
                lambda base, seen: (
                    seen.mean()[1] > seen.mean()[0] + 8 and seen.mean()[1] > seen.mean()[2] + 8,
                    "mean=%s" % (tuple(round(v) for v in seen.mean()),),
                ),
            ),
        ],
        "ovs.crop": [
            (
                "cropped on the left goes black",
                {"left_px": {"type": "float", "value": float(half)}},
                lambda base, seen: (
                    seen.dark_fraction(right=0.45) > 0.97,
                    "dark left=%.2f right=%.2f"
                    % (seen.dark_fraction(right=0.45), seen.dark_fraction(left=0.55)),
                ),
            ),
        ],
        "ovs.mirror": [
            (
                "mirrored from the left is symmetrical",
                {"horizontal": {"type": "choice", "value": "left"}},
                lambda base, seen: (
                    seen.folded() < 2.0 and base.folded() > 4.0,
                    "fold %.2f -> %.2f" % (base.folded(), seen.folded()),
                ),
            ),
        ],
        "ovs.rgb_split": [
            (
                "pulls the channels apart",
                {"offset_px": {"type": "float", "value": 24.0}},
                # Counted in colours rather than in how far red sits from blue:
                # a yellow caption on a blue ground is as far apart as those two
                # channels get, and shifting them cannot widen it. What a split
                # does make is colours that were in neither.
                lambda base, seen: (
                    seen.colours() > base.colours() * 1.3,
                    "colours %d -> %d" % (base.colours(), seen.colours()),
                ),
            ),
        ],
        "ovs.pixelate": [
            (
                "makes blocks",
                {"block_px": {"type": "float", "value": 32.0}},
                # Thirty-two composition pixels are eight in the preview, which
                # is a quarter the size -- so whole blocks land on whole blocks
                # and "one colour per block" is a question that can be asked.
                lambda base, seen: (
                    seen.uniform_blocks(8) > base.uniform_blocks(8) + 0.1,
                    "blocks of one colour %.2f -> %.2f"
                    % (base.uniform_blocks(8), seen.uniform_blocks(8)),
                ),
            ),
        ],
        "ovs.mask": [
            (
                "the black half of the stencil takes the picture away",
                {
                    "image": {"type": "text", "value": STENCIL["id"] or ""},
                    "amount": {"type": "float", "value": 1.0},
                },
                lambda base, seen: (
                    seen.dark_fraction(right=0.45) > 0.97 and seen.dark_fraction(left=0.55) < 0.2,
                    "dark left=%.2f right=%.2f"
                    % (seen.dark_fraction(right=0.45), seen.dark_fraction(left=0.55)),
                ),
            ),
            (
                "inverted, it takes the other half",
                {
                    "image": {"type": "text", "value": STENCIL["id"] or ""},
                    "invert": {"type": "bool", "value": True},
                },
                lambda base, seen: (
                    seen.dark_fraction(left=0.55) > 0.97 and seen.dark_fraction(right=0.45) < 0.2,
                    "dark left=%.2f right=%.2f"
                    % (seen.dark_fraction(right=0.45), seen.dark_fraction(left=0.55)),
                ),
            ),
            (
                "amount at nought leaves the picture alone",
                {
                    "image": {"type": "text", "value": STENCIL["id"] or ""},
                    "amount": {"type": "float", "value": 0.0},
                },
                lambda base, seen: (
                    distance(base, seen)[0] <= SAME,
                    "distance=%.2f" % distance(base, seen)[0],
                ),
            ),
        ],
        "ovs.blur": [
            (
                "softens edges",
                {"radius_px": {"type": "float", "value": 48.0}},
                lambda base, seen: (
                    seen.edginess() < base.edginess() * 0.8,
                    "edges %.2f -> %.2f" % (base.edginess(), seen.edginess()),
                ),
            ),
        ],
    }
    if not STENCIL["id"]:
        # Nothing was uploaded, so there is nothing to mask with. Dropped rather
        # than run against no picture, which would report the Mask effect as
        # broken when what is missing is the stencil.
        knows.pop("ovs.mask", None)
    return knows


def longest_still_run(count=20, pause=0.19):
    """The longest stretch of looks that were all the same picture.

    The one honest way to ask this preview about a flash rate. The rate itself
    cannot be counted -- a flash can begin and end between two looks -- but a
    slow strobe holds one picture for several looks together and a fast one
    never does, and that difference is large rather than marginal.
    """
    seen = samples(count, pause)
    longest, run = 1, 1
    for one, other in zip(seen, seen[1:]):
        run = run + 1 if one.mark == other.mark else 1
        longest = max(longest, run)
    return longest


def strobe_checks(report, route):
    """A strobe, asked about at a rate this machine can draw.

    **Not at its default.** With no output attached the render loop wakes every
    hundred milliseconds (`render_loop::next_deadline`), so the composition is
    drawn ten times a second -- and a strobe at its default ten hertz is drawn
    at the same point of every flash. It really is a still picture here, and no
    amount of sampling finds a flash that was never composed. With a wall on
    the machine the loop runs at the wall's rate and the question answers
    itself.
    """
    flashing = {
        "hz": {"type": "float", "value": 1.0},
        "colour": {"type": "color", "value": [1.0, 0.0, 0.0, 1.0]},
    }
    call("PUT", route, {"reset": True, "overrides": flashing})
    pushed()
    seen = samples(14, 0.19)
    brightest = max(seen, key=lambda one: one.mean()[0])
    darkest = min(seen, key=lambda one: sum(one.mean()[:3]))
    report.check(
        "flashes, and the colour is the one set live",
        brightest.mean()[0] > 150 and brightest.mean()[0] > brightest.mean()[1] * 2 + 8,
        "brightest %s at 1 Hz" % (tuple(round(v) for v in brightest.mean()),),
    )
    report.check(
        "goes dark between flashes",
        sum(darkest.mean()[:3]) < 30,
        "darkest %s" % (tuple(round(v) for v in darkest.mean()),),
    )

    # Both rates below the ten-a-second the loop draws at, or the faster one is
    # aliased into a still picture and the measurement says nothing.
    call("PUT", route, {"reset": True, "overrides": {"hz": {"type": "float", "value": 0.5}}})
    pushed()
    slow = longest_still_run()
    call("PUT", route, {"reset": True, "overrides": {"hz": {"type": "float", "value": 4.0}}})
    pushed()
    fast = longest_still_run()
    call("PUT", route, {"reset": True})
    told_apart = slow >= fast * 2 and slow >= 3
    report.check(
        "hz does something",
        # Not a failure when it cannot be told: with the composition drawn ten
        # times a second and this endpoint handing out four or five pictures a
        # second, four hertz is already past what can be resolved. The two
        # settings are accepted and do give different figures; which is which
        # is a question for a machine with a wall on it.
        True if told_apart else None,
        "longest unchanged stretch: %d looks at 0.5 Hz, %d at 4 Hz%s"
        % (slow, fast, "" if told_apart else " -- too close to call at ten frames a second"),
    )


def shape_rotation(report, route):
    """A rectangle that is not square, turned a quarter of the way.

    The turn cannot be seen on anything the generic sweep reaches for: the
    default shape is a circle, and everything switched on is a square larger
    than the frame. Both turn into themselves.
    """
    oblong = {
        "shape": {"type": "choice", "value": "rectangle"},
        "width_px": {"type": "float", "value": 1200.0},
        "height_px": {"type": "float", "value": 300.0},
    }
    upright = dict(oblong, rotation_deg={"type": "float", "value": 0.0})
    turned = dict(oblong, rotation_deg={"type": "float", "value": 45.0})
    call("PUT", route, {"reset": True, "overrides": upright})
    pushed()
    before = Setting(count=2)
    call("PUT", route, {"reset": True, "overrides": turned})
    pushed()
    after = Setting(count=2)
    call("PUT", route, {"reset": True})
    ok, detail = tell_apart(before, after)
    report.check("rotation_deg does something", ok, "1200x300 upright against 45 degrees, %s" % detail)


# Checks for what the generic sweep cannot reach. The second set names the
# modules the sweep is no use for at all: a strobe's picture is either a flash
# or the gap between two, so "everything switched on" is not a question about it.
GENERATOR_SPECIFIC = {"ovs.strobe": strobe_checks, "ovs.shape": shape_rotation}
SWEEP_SKIPS = {"ovs.strobe"}
# Modules whose movement and whose colour are asked about by a check of their
# own, because the generic ones cannot see them on a machine drawing ten frames
# a second.
JUDGED_ELSEWHERE = {"ovs.strobe"}
COVERED = {
    "ovs.shape": frozenset({"rotation_deg"}),
    # Multiplying and cutting out are two pictures only where there is a layer
    # underneath, and this check builds one layer over black -- where both leave
    # the same black hole. What tells them apart is the pair of layers in
    # `cutting_out_shows_the_layer_underneath_where_multiplying_blacks_it`,
    # which runs on the card. Reporting it here would be reporting the shape of
    # this check rather than anything about the effect.
    "ovs.mask": frozenset({"mode"}),
}


def channel_spread(picture):
    """How far apart red and blue sit. Rises when the channels are shifted."""
    total, count = 0, 0
    for pixel in picture.box():
        total += abs(pixel[0] - pixel[2])
        count += 1
    return total / max(count, 1)


# --- driving the layer ------------------------------------------------------


def blackout(layer_id):
    """Puts the layer on the black slot and waits until the picture is black."""
    call("POST", "/live/layers/%s/slot" % layer_id, {"slot_index": 0})
    return wait_for(is_black)


def show_slot(layer_id, index=1):
    """Selects a slot and waits until there is something other than black."""
    call("POST", "/live/layers/%s/slot" % layer_id, {"slot_index": index})
    return wait_for(lambda one: not is_black(one))


def sweep(report, route, manifest, neutral, covered=frozenset()):
    """Pushes every parameter the module declares, one at a time.

    Each one is tried on its own first, over the module's own defaults. Where
    that cannot show anything the other way round is tried: everything switched
    on, and this one parameter put back. Both are needed. A parameter that is
    already at its enabling end by default -- blur's amount -- shows nothing on
    its own, and a parameter the defaults make no difference to -- the turn of a
    circle -- needs the rest switched on before it can be seen at all. A
    parameter neither way can show is one nothing can see.
    """
    on_values, off_values, masters = {}, {}, {}
    for parameter in manifest["parameters"]:
        on = enabling_value(parameter)
        if on is None:
            continue
        off = opposite_value(parameter, on)
        if off is None or off == on:
            continue
        on_values[parameter["key"]] = on
        off_values[parameter["key"]] = off
        if parameter["key"] in MASTERS:
            masters[parameter["key"]] = on
    if not on_values:
        return None

    complaint = refused("PUT", route, {"reset": True, "overrides": on_values})
    if complaint:
        report.check("every value accepted", False, complaint)
        return None
    pushed()
    full = Setting()
    ok, detail = tell_apart(neutral, full)
    report.check("full up does something", ok, detail)

    for key, on in on_values.items():
        if key in covered:
            # Something that knows this parameter has already asked about it.
            continue
        alone = dict(masters)
        alone[key] = on
        complaint = refused("PUT", route, {"reset": True, "overrides": alone})
        if complaint:
            report.check("%s accepted" % key, False, complaint)
            continue
        pushed()
        ok, detail = tell_apart(neutral, Setting())
        if not ok:
            overrides = dict(on_values)
            overrides[key] = off_values[key]
            complaint = refused("PUT", route, {"reset": True, "overrides": overrides})
            if complaint:
                report.check("%s accepted" % key, False, complaint)
                continue
            pushed()
            ok, other = tell_apart(full, Setting())
            detail = "%s, put back %s" % (detail, other)
        report.check(
            "%s does something" % key,
            True if ok else None,
            "%s=%s %s" % (key, on["value"], detail),
        )
    call("PUT", route, {"reset": True})
    pushed()
    return full

# --- the run ----------------------------------------------------------------


def base_source(width, height):
    """A still, lopsided, colourful picture: what the effects are judged against.

    Lopsided on purpose. A mirror of a symmetrical picture is the picture, and
    an effect that did nothing would pass.
    """
    return {
        "kind": "generator",
        "instance_id": ulid(),
        "type_id": "ovs.text",
        "version": 1,
        "parameters": {
            "text": {"type": "text", "value": "OVS PRUEFBILD 1234"},
            "cap_px": {"type": "float", "value": round(height * 0.16)},
            "weight": {"type": "float", "value": 0.3},
            "tracking": {"type": "float", "value": 0.2},
            "align": {"type": "choice", "value": "left"},
            "text_colour": {"type": "color", "value": [0.95, 0.82, 0.18, 1.0]},
            "background_colour": {"type": "color", "value": [0.09, 0.11, 0.38, 1.0]},
        },
    }


def main():
    report = Report()
    status = call("GET", "/server/status")
    telemetry = status.get("telemetry", {})
    version = telemetry.get("version", "?")
    was_open = telemetry.get("current_show")
    previews_were = status.get("previews_enabled", True)

    print("OVS %s on %s" % (version, telemetry.get("ip_address", "?")))
    print("open show: %s (%s)" % (telemetry.get("current_show_name"), was_open))
    print("previews: %s   outputs: %d" % (previews_were, len(telemetry.get("outputs", []))))

    if not previews_were:
        call("PUT", "/preview/enabled", {"enabled": True})

    # An argument narrows the run to the modules whose name contains it, so one
    # module can be looked at again without sitting through all of them.
    only = sys.argv[1] if len(sys.argv) > 1 else ""
    generators = [one for one in call("GET", "/generators")["generators"] if only in one["type_id"]]
    effects = [one for one in call("GET", "/effects")["effects"] if only in one["type_id"]]
    print("catalogue: %d generators, %d effects" % (len(generators), len(effects)))
    rate, asked = preview_rate()
    print("preview: %.1f new pictures a second at %.1f looks a second" % (rate, asked))

    layer_id = ulid()
    show = call("POST", "/shows", {"name": "Module check"})
    show_id = show["id"]
    if "layers" not in show:
        show = call("GET", "/shows/" + show_id)
    size = show.get("composition", {}).get("size", {})
    width = size.get("width", 1920)
    height = size.get("height", 1080)
    print("composition: %dx%d" % (width, height))
    sys.stdout.flush()

    # A stencil for the Mask effect, which is the one module that is driven
    # with a picture rather than with numbers. Uploaded before the show is
    # built, so that everything below can name it.
    try:
        STENCIL["width"], STENCIL["height"] = width, height
        STENCIL["id"] = upload(
            "Module check stencil",
            png_of(width, height, lambda x, _: (0, 0, 0, 255) if x < width // 2 else (255,) * 4),
            width,
            height,
        )
        print("stencil: %s" % STENCIL["id"])
    except Exception as error:  # noqa: BLE001 - reported, not handled
        print("no stencil, so the Mask effect is driven with nothing: %s" % error)

    try:
        # Opened before it is edited, because a show that is not loaded cannot
        # be edited -- the server answers 409 to a save on any other document.
        call("POST", "/shows/%s/open" % show_id, None)
        show["layers"] = [
            {
                "id": layer_id,
                "name": "Check layer",
                "order": 0,
                "enabled": True,
                "route": "M",
                "default_opacity": 1.0,
                "default_transform": {
                    "x": 0.0,
                    "y": 0.0,
                    "scale_x": 1.0,
                    "scale_y": 1.0,
                    "rotation_deg": 0.0,
                },
                "blend_mode": "normal",
                "banks": 1,
                "slots": [],
                "effects": [],
            }
        ]
        call("PUT", "/shows/" + show_id, {"show": show})
        black = blackout(layer_id)
        print("black: %s" % black)
        if not is_black(black):
            print("WARNING: the black slot is not black, so everything after this is shaky")

        run_generators(report, show_id, layer_id, generators)
        run_effects(report, show_id, layer_id, effects, width, height)
        if not only:
            run_retired(report, show_id, layer_id)
    finally:
        print()
        print("putting it back")
        if was_open:
            print(" ", refused("POST", "/shows/%s/open" % was_open, None) or "the show is open again")
        print(" ", refused("DELETE", "/shows/" + show_id) or "the test show is deleted")
        if STENCIL["id"]:
            # Out of the library as well. A check that left a picture behind
            # every time it ran would be a check that tidies up after the show
            # and not after itself.
            print(
                " ",
                refused("DELETE", "/media/" + STENCIL["id"]) or "the stencil is out of the library",
            )
        if not previews_were:
            call("PUT", "/preview/enabled", {"enabled": False})

    report.summary()
    return 1 if report.failed else 0


def stored_source(show_id, layer_id, slot_index):
    """The slot as the database has it."""
    show = call("GET", "/shows/" + show_id)
    for layer in show["layers"]:
        if layer["id"] != layer_id:
            continue
        for slot in layer["slots"]:
            if slot["index"] == slot_index:
                return slot["source"]
    return {}


def run_generators(report, show_id, layer_id, generators):
    report.section("GENERATORS")
    for manifest in generators:
        type_id, version = manifest["type_id"], manifest["version"]
        report.module("%s v%d  %s" % (type_id, version, manifest["name"]))

        # The identity is minted here, the way the client mints it when a
        # generator is dragged onto a pad (`editor.ts`). Whether the server
        # mints one when nobody sends it is checked on its own below.
        instance_id = ulid()
        source = {
            "kind": "generator",
            "instance_id": instance_id,
            "type_id": type_id,
            "version": version,
            "parameters": defaults_of(manifest),
        }
        blackout(layer_id)
        complaint = refused(
            "PUT",
            "/shows/%s/layers/%s/slots/1" % (show_id, layer_id),
            {"source": source},
        )
        if complaint:
            report.check("put in the slot", False, complaint)
            continue
        show_slot(layer_id, 1)

        drawn = Setting(count=9, pause=0.28)
        report.check("draws something", not is_black(drawn.first), repr(drawn.first))
        brightest = max(drawn.frames, key=lambda one: sum(one.mean()[:3]))
        generator_shape(report, type_id, brightest)

        if type_id in JUDGED_ELSEWHERE:
            pass
        elif (type_id, version) in FREE_RUNNING:
            report.check("runs on its own", drawn.moving, "movement=%.2f" % drawn.movement)
        elif (type_id, version) in STANDS_STILL:
            report.check("stands still", not drawn.moving, "movement=%.2f" % drawn.movement)
        else:
            report.check("movement", None, "%.2f (nothing expected either way)" % drawn.movement)

        kept = stored_source(show_id, layer_id, 1).get("instance_id")
        if kept != instance_id:
            report.check("identity stored", False, "sent %s, read back %s" % (instance_id, kept))
            continue
        route = "/live/generators/" + instance_id

        # Every colour this generator has, live, at once. Whatever it draws, if
        # all of it is one colour then the whole frame is that colour -- which
        # works the same for a grid, a shape, a clock or a gradient.
        colours = colours_of(manifest, [1.0, 0.0, 0.0, 1.0])
        if colours and type_id not in JUDGED_ELSEWHERE:
            complaint = refused("PUT", route, {"reset": True, "overrides": colours})
            if complaint:
                report.check("colour set live", False, complaint)
            else:
                pushed()
                # The reddest frame rather than the brightest: white is as red
                # as red is, so brightness alone cannot tell them apart.
                reddest = max(
                    samples(5, 0.22),
                    key=lambda one: one.mean()[0] - (one.mean()[1] + one.mean()[2]) / 2,
                )
                mean = reddest.mean()
                report.check(
                    "red when set live",
                    mean[0] > 40 and mean[0] > mean[1] * 2 + 8 and mean[0] > mean[2] * 2 + 8,
                    "mean=%s" % (tuple(round(v) for v in mean),),
                )
            call("PUT", route, {"reset": True})
            pushed()

        if type_id in GENERATOR_SPECIFIC:
            # What the generic sweep cannot reach, asked on the module's terms.
            GENERATOR_SPECIFIC[type_id](report, route)
        if type_id not in SWEEP_SKIPS:
            # After whatever the check above left on the module, and before the
            # picture everything else is measured against.
            pushed()
            neutral = Setting()
            sweep(
                report,
                route,
                dropped_colours(manifest),
                neutral,
                COVERED.get(type_id, frozenset()),
            )

    # One door, one rule: a generator that arrives without an identity has to
    # be given one where it is stored, or a patch entry in the same document
    # points at a generator that will not exist after the next open. The
    # whole-document save does this; this is the other door.
    report.module("the slot door itself")
    complaint = refused(
        "PUT",
        "/shows/%s/layers/%s/slots/2" % (show_id, layer_id),
        {"source": {"kind": "generator", "type_id": "ovs.solid_colour", "version": 1}},
    )
    if complaint:
        report.check("accepted with no identity", False, complaint)
    else:
        kept = stored_source(show_id, layer_id, 2).get("instance_id")
        report.check("identity filled in", bool(kept), "stored: %s" % (kept or "none"))


def dropped_colours(manifest):
    """The manifest without its colours, which were pushed together already."""
    return {
        "type_id": manifest["type_id"],
        "version": manifest["version"],
        "parameters": [
            one for one in manifest["parameters"] if one["parameter_type"]["kind"] != "colour"
        ],
    }


# Types this build no longer offers and must still serve, so that a show built
# before they were retired still opens and still draws. Spelled out here
# because they are not in the catalogue by definition -- the lists are
# `config::generators::RETIRED` and `config::effects::RETIRED`, and the values
# are the defaults those manifests publish.
RETIRED_GENERATORS = [
    (
        "ovs.clock",
        1,
        {
            "hour_24": {"type": "bool", "value": True},
            "show_seconds": {"type": "bool", "value": True},
            "text_colour": {"type": "color", "value": [1.0, 1.0, 1.0, 1.0]},
            "background_colour": {"type": "color", "value": [0.0, 0.0, 0.0, 1.0]},
            "utc_offset_minutes": {"type": "float", "value": 0.0},
        },
    ),
    (
        "ovs.countdown",
        1,
        {
            "duration_s": {"type": "float", "value": 600.0},
            "from_trigger": {"type": "bool", "value": True},
            "count_past_zero": {"type": "bool", "value": False},
            "text_colour": {"type": "color", "value": [1.0, 1.0, 1.0, 1.0]},
            "background_colour": {"type": "color", "value": [0.0, 0.0, 0.0, 1.0]},
        },
    ),
]
RETIRED_EFFECTS = [("ovs.tint", 1)]


def run_retired(report, show_id, layer_id):
    """What this build no longer offers, it still has to serve.

    A type is retired by taking it out of the catalogue, never out of the
    build: a show built last week names it, and that show has to open and draw
    exactly as it did. Nothing else in this run would notice if that stopped,
    because everything else is driven from the catalogue.
    """
    report.section("RETIRED, AND STILL SERVED")
    for type_id, version, parameters in RETIRED_GENERATORS:
        report.module("%s v%d" % (type_id, version))
        blackout(layer_id)
        complaint = refused(
            "PUT",
            "/shows/%s/layers/%s/slots/1" % (show_id, layer_id),
            {
                "source": {
                    "kind": "generator",
                    "instance_id": ulid(),
                    "type_id": type_id,
                    "version": version,
                    "parameters": parameters,
                }
            },
        )
        report.check("still goes in a slot", complaint is None, complaint or "")
        if complaint:
            continue
        show_slot(layer_id, 1)
        drawn = Setting(count=3)
        report.check("still draws", not is_black(drawn.first), repr(drawn.first))

    for type_id, version in RETIRED_EFFECTS:
        report.module("%s v%d" % (type_id, version))
        try:
            added = call(
                "POST",
                "/shows/%s/layers/%s/effects" % (show_id, layer_id),
                {"type_id": type_id, "version": version},
            )
        except urllib.error.HTTPError as error:
            report.check("still goes on a layer", False, "%d %s" % (error.code, error.read()[:200]))
            continue
        report.check("still goes on a layer", True, "")
        effect_id = added["effect_instance_id"]
        route = "/live/effects/" + effect_id
        pushed()
        before = Setting(count=2)
        complaint = refused(
            "PUT",
            route,
            {
                "reset": True,
                "overrides": {
                    "amount": {"type": "float", "value": 1.0},
                    "colour": {"type": "color", "value": [1.0, 0.0, 0.0, 1.0]},
                },
            },
        )
        if complaint:
            report.check("still takes its values", False, complaint)
        else:
            pushed()
            ok, detail = tell_apart(before, Setting(count=2))
            report.check("still takes its values", ok, detail)
        call(
            "DELETE",
            "/shows/%s/layers/%s/effects/%s" % (show_id, layer_id, effect_id),
        )


def run_effects(report, show_id, layer_id, effects, width, height):
    report.section("EFFECTS")
    blackout(layer_id)
    complaint = refused(
        "PUT",
        "/shows/%s/layers/%s/slots/1" % (show_id, layer_id),
        {"source": base_source(width, height)},
    )
    if complaint:
        report.module("the base picture")
        report.check("base picture put in place", False, complaint)
        return
    show_slot(layer_id, 1)
    base = Setting()
    print(
        "  base picture: %s edges=%.2f runs=%.1f greyness=%.2f fold=%.2f movement=%.2f"
        % (
            base.first,
            base.first.edginess(),
            base.first.run_length(),
            base.first.greyness(),
            base.first.folded(),
            base.movement,
        )
    )
    sys.stdout.flush()
    if base.first.flat() or base.moving:
        report.module("the base picture")
        report.check(
            "the base picture stands still and has something in it",
            False,
            "%s movement=%.2f" % (base.first, base.movement),
        )
        return

    knows = specific(width, height)
    for manifest in effects:
        type_id, version = manifest["type_id"], manifest["version"]
        report.module("%s v%d  %s" % (type_id, version, manifest["name"]))

        try:
            added = call(
                "POST",
                "/shows/%s/layers/%s/effects" % (show_id, layer_id),
                {"type_id": type_id, "version": version},
            )
        except urllib.error.HTTPError as error:
            report.check("angehaengt", False, "%d %s" % (error.code, error.read()[:200]))
            continue
        effect_id = added["effect_instance_id"]
        route = "/live/effects/" + effect_id

        pushed()
        try:
            # Neutral at its published defaults. The one rule every effect in
            # the catalogue has to keep: adding it changes nothing until it is
            # asked to.
            with_effect = Setting()
            apart, worst = distance(base.first, with_effect.first)
            report.check(
                "neutral at its published defaults",
                apart <= SAME,
                "distance=%.2f worst=%.0f" % (apart, worst),
            )

            full = sweep(report, route, manifest, with_effect)
            if type_id == "ovs.tv_glitch" and full is not None:
                report.check("keeps disturbing", full.moving, "movement=%.2f" % full.movement)

            for label, overrides, predicate in knows.get(type_id, []):
                complaint = refused("PUT", route, {"reset": True, "overrides": overrides})
                if complaint:
                    report.check(label, False, complaint)
                    continue
                pushed()
                seen = Setting(count=2).first
                ok, detail = predicate(base.first, seen)
                report.check(label, ok, detail)
                call("PUT", route, {"reset": True})
        finally:
            call("DELETE", "/shows/%s/layers/%s/effects/%s" % (show_id, layer_id, effect_id))

        # Taking it off puts the picture back where it was.
        pushed()
        back = Setting(count=2)
        apart = distance(base.first, back.first)[0]
        report.check("taken off again, as it was", apart <= SAME, "distance=%.2f" % apart)


if __name__ == "__main__":
    sys.exit(main())
