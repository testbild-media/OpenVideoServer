# dot2 fixture types

The **same files as [`../grandma2/`](../grandma2/README.md)**, byte for byte: a
dot2 reads a grandMA2's fixture types. MA's own Fixture Builder writes one file
and puts it in both libraries. Read that README for what is in them and how
they are laid out. Generated with

```
cargo run -p ovs-proto --bin ovs-desk-files
```

and held to the catalogue by the same golden test. The server's fixture ZIP has
them under `dot2/`.

**Not yet imported on a dot2** (2026-10-21).

## Importing

1. On the USB stick, put the `.xml` files in **`dot2/library`**. A grandMA2's
   `gma2` folder is not looked in.
2. On the dot2: **Setup → Patch and Fixture Schedule → Add New Fixtures →
   Select other…**, choose **USB**, and import each type. Or hold a fixture
   type cell and tap **Import**.

Fixture types imported into the dot2's own library are wiped by a software
update or a factory reset. Keep the stick.

## What is particular to a dot2

- **The header.** A dot2 ignores, with no message, a file tagged newer than
  itself, and its last release is 1.9. These carry the header MA's Fixture
  Builder 1.9 writes, `major_vers="3" minor_vers="2" stream_vers="2"`.
- **The owner's own attributes** (`PARA_1` to `PARA_8`, `OVS_ROTATION`,
  `OVS_SCALE_X`/`Y`, `OVS_ALPHA`, `OVS_BG_…`). A grandMA2 makes an attribute
  it does not have when a type brings one; whether a dot2 does the same has not
  been seen. If one is missing after the import, that is the first thing to
  report.
- A dot2 has no fixture type editor, so a channel that should snap (a layer's
  slot) cannot be changed on it.
