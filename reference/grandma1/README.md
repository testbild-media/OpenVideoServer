# grandMA1 fixture types

One file per OpenVideoServer DMX personality, for a grandMA1 (console or onPC
6.x), manufacturer **testbild.media**. Generated from the same catalogue as
[`../FIXTURES.md`](../FIXTURES.md) — do not edit by hand; run

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
nothing changed: the desk still lists them under `testbild.media`.

## Names

| On the desk | What it is | Patch in OVS to |
|---|---|---|
| `OVS Master` | the show master | the master, once per show |
| `OVS Layer` | one layer, with every slot named | one layer |
| `OVS FX …` | one effect | one effect instance in a chain |
| `OVS Gen …` | one generator | one generator instance in a slot |

Version one of a module carries no number and later versions end in `v2`,
`v3`: a name already in a show file never has to change. Retired modules
(Tint, Clock, Countdown) have no file, because nothing can be given one any
more.

## Where things sit

A control takes one of the desk's own attributes only where a tool of the
desk answers to it, and then exactly as MA's library writes it:

- **Layer position is `PAN` and `TILT`**, so the trackball moves a layer and
  the desk's position effects (circles and the like) run on it. Pan is
  across, tilt is up: a larger tilt moves the picture up the wall. If it goes
  the wrong way on a desk, invert tilt in the patch.
- **Every dimmer is `DIM`**: the master's, each layer's, and in a module the
  control that says how much of it there is (below). The grand master, a
  group master, Highlight and `At` all act on it.
- **The first colour of a module is `RED`/`GREEN`/`BLUE`** with the colour-mix
  effect, so the colour picker drives it.
- **A layer's turn and size** are `Z ROT` and `X SCALE`/`Y SCALE` under
  Pan/Tilt, as the library's media servers have them. Home on the scales is
  exactly 1.

**Everything else is ours and says so.** An attribute is `OVS`, the module and
the control — `OVS BLUR RADIUS`, `OVS CC SATURATION` — in a group that is `OVS`
and the module: `OVS BLUR`, `OVS COLOUR CORRECT`. The layer's slot is
`OVS SLOT` under Gobo, with `Black` and `Slot 1` to `Slot 84` as named values
at the exact bands the server reads. A module's alpha is `OVS ALPHA` beside its
red, green and blue; a background is `OVS BG RED` and so on in
`OVS BACKGROUND`, whichever generator it belongs to; the far end of a gradient
is `OVS TO RED` in `OVS GRADIENT TO`. No attribute of ours means two things in
two modules, so a show that keeps one list of attributes cannot mix them up.

Choices and switches snap, and every step is a named value with the name the
client shows. Every channel's home is the fixture's default, so a freshly
patched layer is visible, full size and upright, and a freshly patched effect
changes nothing.

## A group master for the effects

Every module that has an `Amount` has it as its dimmer, and four more have
their strength there:

| Module | Its dimmer | At nought |
|---|---|---|
| Blur, Colorize, Edges, Mask, TV Glitch, Noise | Amount | none of it |
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
