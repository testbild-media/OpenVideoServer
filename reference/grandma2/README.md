# grandMA2 fixture types

One file per thing a desk can patch on OpenVideoServer, for a grandMA2 (console
or onPC), manufacturer **testbild.media**. The **same files** are in
[`../dot2/`](../dot2/), because a dot2 reads a grandMA2's fixture types. They
are generated from the same catalogue as [`../FIXTURES.md`](../FIXTURES.md) and
the [grandMA1 files](../grandma1/README.md). Do not edit them by hand; run

```
cargo run -p ovs-proto --bin ovs-desk-files
```

and commit the result. A golden test fails when the files and the server
disagree. The server's status page also offers them, in its fixture ZIP, under
`grandMA2/`.

**Not yet imported on a console** (2026-10-21). They are written to the format
of real grandMA2 exports and MA's Fixture Builder; see
[What to check first](#what-to-check-first).

## Importing

1. On the USB stick, put the `.xml` files in **`gma2/library`**. Opening the
   Backup menu with the stick in, or exporting any fixture type to it, makes
   the folders.
2. On the desk: **Setup → Patch & Fixture Schedule → Add new fixture →
   Import Fixture Type**. Choose the stick under "Select Drive", filter by
   `testbild.media` and import each type.
   - From the command line, in Edit Setup: `EditSetup/FixtureTypes>` then
     `Import "testbild_media@ovs_layer@standard"`, with no extension.

## How they are written

- **The Fixture Builder 1.9 header**: `major_vers="3" minor_vers="2"
  stream_vers="2"`, which a grandMA2 takes and which a dot2 still takes. A
  dot2 ignores, with no message, a file tagged newer than itself.
- One module, one instance at `patch="1"`, one mode called `Standard`.
- A sixteen-bit channel is `coarse` and `fine`. Defaults and named values are
  in the channel's own resolution (0–255, or 0–65535 for a wide channel), and a
  function's range is 24-bit, as every grandMA2 file has it.
- **Named values are ranges**, exactly the grandMA1's: Black is 0 to 3 and every
  slot its three values, a choice its share of the channel, a landmark of a
  continuous channel the eight-bit step it is in. A dimmer or a colour has none.
- A channel's physical range is the control's own (`physfrom`/`physto`), so a
  hue reads in degrees and a temperature in kelvin.

## Attributes

The owner's layout from the grandMA1 (2026-10-18), with the desk's own
attributes where the grandMA2 has one and the owner's names where it does not:

| Role | Attribute | Feature | Preset |
|---|---|---|---|
| dimmer (a module's amount) | `DIM` | DIMMER | Dimmer |
| X, Y | `PAN`, `TILT` | POSITION | Position |
| rotation | `OVS_ROTATION` | POSITION | Position |
| scale | `OVS_SCALE_X`, `OVS_SCALE_Y` | OVS_SCALE | Position |
| parameter 1 to 8 | `PARA_1` … `PARA_8` | OVS_PARAMETER | Video |
| foreground colour | `COLORRGB1`, `COLORRGB2`, `COLORRGB3` | COLORRGB | Color |
| its alpha | `OVS_ALPHA` | OVS_ALPHA | Color |
| background | `OVS_BG_RED` … `OVS_BG_ALPHA` | OVS_RGBX_BG | Color |

A grandMA2 makes an attribute it does not have when a fixture type brings one,
with the feature and preset the file gives it, and leaves one it has where it
is. So the owner's names carry the `OVS_` prefix where a grandMA2 may already
have the plain word (a "Scale" of its own landed in the desk's Video group for
somebody, MA forum). The alpha is its own feature rather than a fourth colour:
a colour picker that took it for an emitter would set it to nought for every
saturated colour and make the picture transparent.

## What to check first

In this order, because each depends on the one before:

1. **That a type imports at all**, and shows up under `testbild.media`.
2. **That the owner's attributes are made**: Parameter 1 to 8 under
   **Video**, Scale under Position, the background under Color. The XML word
   for the Video preset, `VIDEO`, is the grandMA1's and was not found in any
   grandMA2 file.
3. **A layer's slot**: whether a cue fading into another slot plays the slots
   in between. The grandMA1 file snaps that channel; no grandMA2 file showed
   how to write a snap, so these do not. If it fades, set the channel to snap
   in the fixture type editor.
4. **The colour picker** on Solid Colour: red, green and blue, with alpha left
   alone.
