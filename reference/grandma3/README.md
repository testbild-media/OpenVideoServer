# grandMA3 fixture types (GDTF)

One **GDTF** file per thing a desk can patch on OpenVideoServer, for a grandMA3
(console or onPC), manufacturer **testbild.media**. Generated from the same
catalogue as [`../FIXTURES.md`](../FIXTURES.md) and the
[grandMA1 files](../grandma1/README.md). Do not edit them by hand; run

```
cargo run -p ovs-proto --bin ovs-desk-files
```

and commit the result. A golden test fails when the files and the server
disagree. The server's status page also offers them, in its fixture ZIP, under
`grandMA3/`.

**Not yet imported on a console** (2026-10-21). Every file passes a checker
built from the GDTF 1.2 specification, its schema and its rules; what a
grandMA3 makes of them has to be seen on one. See
[What to check first](#what-to-check-first).

## Why GDTF

A grandMA3 imports its own XML and GDTF. Its own XML has no published
description, is tied to the console's version, and imports as an empty type
with no message when something in it is wrong. GDTF (DIN SPEC 15800) is
published whole, and every grandMA3 reads it. These are GDTF 1.2.

## Importing

1. On the USB stick, put the `.gdtf` files in
   **`grandMA3/gma3_library/fixturetypes`**. On onPC for Windows the folder is
   `C:\ProgramData\MALightingTechnology\gma3_library\fixturetypes`.
2. On the desk: **Menu → Patch → Fixture Types → Import**. Choose the stick at
   the top right, select each type and tap **Import**.

## How they are written

- **A ZIP with nothing compressed**, holding `description.xml`. A grandMA3
  before 2.4 imports nothing else ("It was not possible to import compressed
  GDTF zip files", release notes 2.4).
- **`.gdtf` in lower case**. Before 2.4 a grandMA3 did not import `*.GDTF`.
- **One geometry, `Body`**, and one mode, `Standard`. Each attribute is on one
  channel only, because a grandMA3 2.2 refused a whole type over two of one in
  a geometry. A test holds every file to that.
- **The same identity in every build** (`FixtureTypeID`, from the module's
  type), so a newer file is the same fixture type to the desk rather than a
  second one.

## Attributes

The owner's layout from the grandMA1 (2026-10-18), in GDTF's own attribute
names wherever GDTF has one:

| Role | Attribute | Feature |
|---|---|---|
| dimmer (a module's amount) | `Dimmer` | Dimmer.Dimmer, on the grand master |
| X, Y | `Pan`, `Tilt` | Position.PanTilt |
| rotation | `Rot_Z` | Position.Rotation, in degrees |
| scale | `Scale_X`, `Scale_Y` | Position.Scale |
| parameter n | `VideoEffect1Parameter1` … | Video.Video |
| foreground colour | `ColorAdd_R`, `_G`, `_B` | Color.RGB |
| its alpha | `OVS_Alpha` | Color.Alpha |
| background | `OVS_BG_R`, `_G`, `_B`, `OVS_BG_Alpha` | Color.Background |

**The parameters are shared across fixtures**, as the grandMA1's `PARA_1` to
`PARA_8` are. A grandMA3 show holds 1,024 attributes, and one per control of
every module would spend them. What a parameter is on a given fixture is its
channel function's name: Blur's Parameter 1 is "Radius", a layer's is "Slot".

The alpha and the background have no GDTF attribute, so they are declared in
the files, in features of their own. An alpha inside GDTF's RGB would be taken
for a fourth emitter by the colour picker, which would set it to nought for
every saturated colour and make the picture transparent.

## Values

- **A step and each side of a switch are channel sets**, which in GDTF run to
  where the next one starts. So a step's set is exactly the band the server
  reads as that step (Black 0–3, Slot 1 from 4), as the grandMA1's are. A
  layer's slot and every stepped channel **snap**.
- A hue's colours are sets too (Red, Orange, …).
- **A continuous channel has no sets.** Its physical range is the control's
  own, so a grandMA3 shows and takes the value in degrees, kelvin or pixels.
  That is what the grandMA1's Min, Center and Max were standing in for.

## What to check first

In this order:

1. **That a type imports** and shows a mode called `Standard` with every
   channel. A type that imports with no DMX mode is the 2.1-era symptom of a
   file the desk could not read.
2. **The layer**: Pan and Tilt on the trackball, Rot Z, Scale X and Y, and the
   slot stepping through Black, Slot 1, Slot 2 without fading through them.
3. **The colour picker** on Solid Colour: red, green and blue, with alpha left
   alone.
4. **Parameter names**: that an effect's encoder says what the parameter is
   (Radius, Hue) and not only "Parameter 1".
