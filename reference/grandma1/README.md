# grandMA1 fixture types

One file per thing a desk can patch on OpenVideoServer, for a grandMA1
(console or onPC 6.x), manufacturer **testbild.media**. Generated from the same
catalogue as [`../FIXTURES.md`](../FIXTURES.md) — do not edit by hand; run

```
cargo run -p ovs-proto --bin ovs-grandma1
```

and commit the result. A golden test fails when the files and the server
disagree.

## Importing

1. Copy the `.TXT` files to a USB stick (any folder).
2. On the desk: **Setup → Full Access → Fixture Types → Import → External**,
   and pick each file.
3. To keep them in the desk's own library as well: select them in Fixture
   Types, **Export → Export Intern**.

**The file names are the desk's rule, not a style.** Capitals, and no dot but
the one before `TXT` — so the manufacturer is `TESTBILD MEDIA` in the name of
every file, as in `TESTBILD MEDIA@OVS FX ECHO.TXT`. A grandMA1 refuses a file
named any other way (found on the first import, 2026-10-18). Inside the files
the desk lists them under `testbild.media`.

## What there is

| On the desk | What it is | Patch in OVS to |
|---|---|---|
| `OVS Master` | the show master, 5 channels | the master, once per show |
| `OVS Layer` | one layer, with every slot named | one layer |
| `OVS FX …` | one effect | one effect instance in a chain |
| `OVS Gen …` | one generator | one generator instance in a slot |

**One of each, and no version in any name.** The owner, 2026-10-18: "Ich habe
aber in der Software keine Versionsvergabe und ist auch nicht gewünscht. TV
Glitch V2 ist TV Glitch." Each file is the module as a client adds it today.
Retired modules (Tint, Clock, Countdown, Grid) have no file, and neither has
Noise, which has no DMX: it is set up in the client and faded with its layer.

The master has no BPM Sync channel any more — the client's phase reset does
that — so BPM Scale is channel 4 and the crossfader channel 5.

## Groups

Six, so a show with every one of these patched lists six groups of ours and
the desk's.

| Group | Preset | Attributes | Pages |
|---|---|---|---|
| Dimmer | Dimmer | `DIM` | 1 |
| Pan/Tilt | Pan/Tilt | `PAN` (X), `TILT` (Y), `OVS ROTATION` (Z) | 1 |
| RGBX | Color | `R`, `G`, `B`, `X` · `BG R`, `BG G`, `BG B`, `BG X` | 2 |
| OVS Layer | Video | `OVS SLOT`, `OVS SCALE X`, `OVS SCALE Y` | 1 |
| OVS Master | Video | `OVS FREEZE`, `OVS BPM TAP`, `OVS BPM SCALE`, `OVS CROSSFADE` | 1 |
| OVS Param | Video | `OVS PARAM 1` to `OVS PARAM 6` | 2 |

- **Pan and tilt are a layer's X and Y**, so the trackball moves a layer and
  the desk's position effects run on it. A larger tilt moves the picture up
  the wall; if it goes the wrong way, invert tilt in the patch.
- **RGBX is a colour of four parts** — red, green, blue and an X that is alpha
  here and could be amber or white on another fixture. The first page is the
  foreground, and its R, G and B are what the colour picker drives; the second
  is the background (`BG`), which for a gradient is its far colour.
- **Everything of ours that is not a colour is under Video**, so the Video
  window keeps presets of it.
- **A module's controls are Param 1 to 6**, shared by every effect and
  generator. The encoder says "Param 1"; what it is in the module you have
  selected is in its named values — Blur's Param 1 reads `Radius 0`,
  `Radius 32`, `Radius 64` — and in the fixture's comment.

Choices and switches snap, and every step is a named value with the name the
client shows. Every channel's home is the fixture's default, so a freshly
patched layer is visible, full size and upright, and a freshly patched effect
changes nothing.

## Every fixture

| Fixture | Dimmer | Param 1 to 6 | RGBX |
|---|---|---|---|
| OVS Master | Master dimmer | — (Freeze, BPM Tap, BPM Scale, Crossfade in OVS Master) | — |
| OVS Layer | Layer dimmer | — (Slot and Scale in OVS Layer, position in Pan/Tilt) | — |
| FX Blur | Amount | Radius | — |
| FX Colorize | Amount | Hue, Saturation | — |
| FX Colour Correct | — | Brightness, Contrast, Saturation, Gamma, Temperature, Tint | — |
| FX Crop | — | Left, Right, Top, Bottom | — |
| FX Echo | Length | Mode | — |
| FX Edges | Amount | Threshold, Line width | foreground |
| FX Invert | Inverted (a switch) | Rate | — |
| FX Mask | Amount | — | — |
| FX Mirror | — | Horizontal, Vertical | — |
| FX Pixelate | Block size | — | — |
| FX RGB Correct | — | Red black, Green black, Blue black, Red white, Green white, Blue white | — |
| FX RGB Split | Offset | Angle | — |
| FX TV Glitch | Amount | Line jumps, Snow, Rolling error, Out of sync | — |
| Gen Gradient | — | Angle | from (foreground), to (background) |
| Gen Shape | — | Shape, Width, Height, Rotation | foreground, background |
| Gen Solid Colour | — | — | foreground |
| Gen Strobe | — | Rate, Mode | foreground |
| Gen Text | — | Cap height, Stroke weight, Tracking, Alignment | foreground, background |

## A group master for the effects

A module's dimmer is the control that says how much of it there is, with
nothing at all at nought:

| Module | Its dimmer | At nought |
|---|---|---|
| Blur, Colorize, Edges, Mask, TV Glitch | Amount | none of it |
| Pixelate | Block size | blocks of one pixel |
| RGB Split | Offset | no split |
| Echo | Length | no trail |
| Invert | Inverted (a switch, on from half up) | not inverted |

So a group of the effect fixtures on a group master takes them all out of the
picture at nought — and puts each back where it was programmed as it comes
up. Colour Correct, RGB Correct, Crop and Mirror have no dimmer, because none
of their controls is "nothing" at nought (no brightness is black); a group
master does not reach them.

**The grand master reaches these dimmers too.** Pulling it down to fade a show
out also pulls the effects' amounts down on the way, and Blackout takes them to
nought with everything else.

## The probe files

`probe/` holds two fixture types that are not for a show. The first set of
files was refused for every fixture with a colour, and the likeliest reason is
that it put a fourth attribute into the desk's own RGB group. RGBX is a group
of its own, but it still has an X beside three colour-mix attributes. Import
`OVS Gen Solid Colour` and both probes, and the three answers say which:

| Solid Colour | Probe RGBX no X | Probe MA RGB | Then |
|---|---|---|---|
| taken | — | — | RGBX works; delete the probes |
| refused | taken | — | X must not sit beside the colour mix |
| refused | refused | taken | the colour mix must be the desk's RED, GREEN, BLUE |
| refused | refused | refused | it is not the colour group at all |

Delete them from the desk and from here once that is answered.
