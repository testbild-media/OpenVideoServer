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

**A fixture's name is eighteen characters at most**, because the owner's desk
cuts it there: Colour Correct is `OVS FX Colour Corr` and Solid Colour is
`OVS Gen Solid Colo`, and their files are named so.

**The files are written the way his desk writes them.** The owner edited every
one on his desk and exported them again on 2026-10-18 ("Alles was du gerade
siehst wurde von der GMA1 wieder exportiert und richtig formatiert"); that
export is [`desk/export-2026-10-18/`](desk/export-2026-10-18/), untouched, and
what was generated from the same build matched it byte for byte except where
noted under [Named values](#named-values). Lines end CR CR LF, and git keeps
them so.

## What there is

| On the desk | What it is | Patch in OVS to |
|---|---|---|
| `OVS Master` | the show master, 5 channels | the master, once per show |
| `OVS Layer` | one layer, with every slot named | one layer |
| `OVS FX …` | one effect | one effect instance in a chain |
| `OVS Gen …` | one generator | one generator instance in a slot |

**One of each, and no version anywhere.** The owner, 2026-10-18: "Ich habe
aber in der Software keine Versionsvergabe und ist auch nicht gewünscht. TV
Glitch V2 ist TV Glitch", and later "Es gibt kein Invert V2.. nirgendwo". The
server publishes one module of each type, so each file is simply that module.
(`_VERSION` in the header is the desk's own field for the file, not ours: his
desk writes the moment it exported, and these say `ORIGINAL`, as MA's own
library does, so that generating them twice gives the same files.)
Retired modules (Tint, Clock, Countdown, Grid) have no file, and neither has
Noise, which has no DMX: it is set up in the client and faded with its layer.

The master has no BPM Sync channel any more — the client's phase reset does
that — so BPM Scale is channel 4 and the crossfader channel 5.

## Groups

**The owner's own, off his desk.** Every attribute and group is spelt as his
desk exported them on 2026-10-18 — underscores, leading ones included — and
[`desk/EXAMPLE.TXT`](desk/EXAMPLE.TXT) is that export. A test holds every file
here to it.

| Group | Preset | Attributes | Labels |
|---|---|---|---|
| DIMMER | Dimmer | `DIM` | Dim |
| PAN/TILT | Pan/Tilt | `PAN` (X), `TILT` (Y), `OVS_ROTATION` (Z) | Pan, Tilt, Rotation |
| SCALE | Pan/Tilt | `OVS_SCALE_X`, `OVS_SCALE_Y` | Scale X, Scale Y |
| OVS_PARAMETER | Video | `PARA_1` to `PARA_8` | Parameter 1 to 8 |
| RGBX | Color | `_RED`, `_GREEN`, `_BLUE`, `_ALPHA` | Red, Green, Blue, Alpha |
| OVS_RGBX_BG | Color | `OVS_BG_RED`, `OVS_BG_GREEN`, `OVS_BG_BLUE`, `OVS_BG_ALPHA` | BG Red to BG Alpha |

- **Pan and tilt are a layer's X and Y**, so the trackball moves a layer and
  the desk's position effects run on it, visualised as a video position on X
  and Y. A larger tilt moves the picture up the wall; if it goes the wrong
  way, invert tilt in the patch.
- **RGBX is the foreground**, and its red, green and blue are what the colour
  picker drives. `_AMBER` and `_WHITE` are in the group on the desk for other
  fixtures; no OVS fixture has them. **OVS_RGBX_BG is the background**, which
  for a gradient is its far colour.
- **Everything else is a parameter, under Video**, so the Video window keeps
  presets of it and no fixture brings a group of its own. The encoder's label
  says "Parameter 1"; beside the value it shows the channel's functional
  block, which is named for what it is on that fixture — "40 Hue",
  "Block size", "Slot" — and so are its named values: Blur's Parameter 1
  reads `Radius 0`, `Radius 32`, `Radius 64`, a layer's `Black`, `Slot 1` to
  `Slot 84`. The fixture's comment lists them all.
- **A control is typed in its own units** where the desk has a way to show
  it — the Visualize column of the functional block — with its real range
  beside it:

  | Control | Visualize | Range |
  |---|---|---|
  | Colorize hue | Hue | 0 to 360° |
  | Colorize and Colour Correct saturation | Color | 0 to 1, 0 to 4 |
  | Colour Correct brightness, contrast | Brightness, Contrast | 0 to 4 |
  | Colour Correct temperature | CTC, Wheel 1 | 2000 to 10000 K |
  | Colour Correct gamma | Brightness | 0.1 to 4 |
  | RGB Correct black and white | Color Boost, R, G and B | −1 to 1, 0 to 4 |
  | Layer rotation | Rotation, Z | −180 to 180° |
  | Shape rotation, RGB Split and Gradient angle | Rotation, Z | 0 to 360° |
  | Layer scale X and Y | Scale, X and Y | 0 to 10 |
  | Shape width and height, Text cap height | Scale, X, Y and All | pixels |
  | Layer position X and Y | Video Position, X and Y | −2 to 2 |
  | Blur radius | Frost | 0 to 64 pixels |
  | Strobe and Invert rate | Strobe, Strobe | hertz |
  | Text stroke weight and tracking | Snd Volume 1 | 0.02 to 0.4, −0.2 to 1 |
  | Shake speed | Speed | 0.5 to 20 new places a second |

  Colorize's hue also has the colours as named values: Red 0, Orange 30,
  Yellow 60, Green 120, Cyan 180, Blue 240, Magenta 300. Gamma has no kind
  of its own on the desk and borrows Brightness, which takes a range. The
  rest — tint, Crop's edges, amounts, thresholds, the choices — is typed as
  a percentage; a tint of 50% is none.

The first set of files was refused for every fixture with a colour, and put a
module's alpha into the desk's own RGB group as a fourth attribute. Nothing
goes there now.

## Named values

**Always a range, never a single value** — the owner, 2026-10-18: "Es ist
immer von bis Wert und kein einzelner Wert." So:

- **A step is its whole band**, exactly the values the server reads as that
  step: a layer's Black is DMX 0 to 3 and every slot its three values, Slot 1
  is 4 to 6 and Slot 84 is 253 to 255; a choice is its even share of the
  channel (Echo's Behind 0 to 127, Lighten 128 to 255); a switch is off below
  half and on from it. Anything the desk sends inside a set is that step.
- **A landmark of a continuous channel is the eight-bit step it is in** —
  `Radius 32` is DMX 128, `Hue` Orange is the coarse step 21 of a sixteen-bit
  channel, 5376 to 5631. On a sixteen-bit channel that is a coarse value and
  every fine one under it, so choosing `Home 1` on a layer's scale lands
  between 0.98 and 1.02; the fixture's default is the exact 1, and Home goes
  back there.
- **Invert's switch on the dimmer** is named at nought and full only, as it is
  in his export.
- They are listed in the order they sit on the channel.

**Where a channel rests.** Every channel's home is the fixture's default, so a
freshly patched layer is visible, full size and upright, and a freshly patched
effect changes nothing. The desk writes no default at nought; anything at full
— a dimmer, a colour — is `100.0000%`; a step rests in the middle of its band,
where his desk put the master's freeze, tap and scale and the layer's slot.

**A module's dimmer rests at nought** — "Wenn dimmer aber Amount ist sollte
default 0 sein" — so a freshly patched effect is out of the picture until it is
brought in. Blur's and Mask's Amount are full in the client, where a blur of
radius nought or a mask with no picture is nothing anyway; on the desk they
start at nought like every other effect's. [`../FIXTURES.md`](../FIXTURES.md)
gives the client's value as their home.

**Where these differed from his export when they were made:**

- **Slots 21 to 84** are ranges too: "Die Slots 0-20 hatte ich gemacht, aber
  bis 84 musst du noch weiterführen."
- **Slot 11 starts at 34**, not 35: his set left DMX 34 out of every slot.
- **Echo's mode, Mirror's two axes, Shape's shape, Strobe's mode and Text's
  alignment rest in the middle of their bands** like the master's steps
  (24.9989% rather than 25.0004%, 16.7956% rather than 16.4065%). The same
  step either way.
- **A colour at full rests at `100.0000%`**, not the `99.6109%` the export
  still had: "default bei Farben auch 100% nicht 99.6109%.. das hatte ich bei
  den Dimmern schon immer geändert."
- `_VERSION` is `ORIGINAL`, as above.

## Every fixture

| Fixture | Dimmer | Parameter 1 to 6 | Colour |
|---|---|---|---|
| OVS Master | Master dimmer | Freeze, BPM Tap, BPM Scale, Crossfade (A, Mix, B) | — |
| OVS Layer | Layer dimmer | Slot (and pan, tilt, rotation, scale in Pan/Tilt) | — |
| FX Blur | Amount | Radius | — |
| FX Colorize | Amount | Hue, Saturation | — |
| FX Colour Corr | — | Brightness, Contrast, Saturation, Gamma, Temperature, Tint | — |
| FX Crop | — | Left, Right, Top, Bottom | — |
| FX Echo | Length | Mode | — |
| FX Edges | Amount | Threshold, Line width | foreground |
| FX Invert | Inverted (a switch) | Rate | — |
| FX Mask | Amount | — | — |
| FX Mirror | — | Horizontal, Vertical | — |
| FX Pixelate | Block size | — | — |
| FX RGB Correct | — | Red black, Green black, Blue black, Red white, Green white, Blue white | — |
| FX RGB Split | Offset | Angle | — |
| FX Shake | Amount | Speed | — |
| FX TV Glitch | Amount | Line jumps, Snow, Rolling error, Out of sync | — |
| Gen Gradient | — | Angle | from (foreground), to (background) |
| Gen Shape | — | Shape, Width, Height, Rotation | foreground, background |
| Gen Solid Colo | — | — | foreground |
| Gen Strobe | — | Rate, Mode | foreground |
| Gen Text | — | Cap height, Stroke weight, Tracking, Alignment | foreground, background |

## A group master for the effects

A module's dimmer is the control that says how much of it there is, with
nothing at all at nought:

| Module | Its dimmer | At nought |
|---|---|---|
| Blur, Colorize, Edges, Mask, TV Glitch | Amount | none of it |
| Shake | Amount | still, and not zoomed |
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
