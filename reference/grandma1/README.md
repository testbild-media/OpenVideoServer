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

1. Copy the `.txt` files to a USB stick (any folder).
2. On the desk: **Setup → Full Access → Fixture Types → Import → External**,
   and pick each file.
3. To keep them in the desk's own library as well: select them in Fixture
   Types, **Export → Export Intern**.

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

- **Dimmers** — the master dimmer and each layer's — are `DIM`.
- **Layer position, turn and size** are `X POS`/`Y POS`, `Z ROT` and
  `X SCALE`/`Y SCALE` under Pan/Tilt, as the media-server fixtures in MA's
  library have them. Each carries named values for its bottom, centre, top
  and home; home on the scales is exactly 1.
- **Slot** is `SLOT` under Gobo, with `Black` and `Slot 1` to `Slot 84` as
  named values at the exact DMX bands the server reads.
- **The first colour of a module** is `RED`/`GREEN`/`BLUE` with the colour-mix
  effect, so the desk's colour picker drives it; its alpha is `ALPHA`. A
  second colour (a background, the far end of a gradient) is named after
  itself, for example `BACKGROUND RED`.
- **Choices and switches** snap, and every step is a named value with the
  name the client shows.
- Everything else is named after the control, under Video, grouped by module.

Every channel's home is the fixture's default, so a freshly patched layer is
visible, full size and upright.
