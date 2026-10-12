# DMX fixture personalities

Generated from the personalities this build publishes. Do not edit by hand:
run `cargo run -p ovs-proto --bin ovs-fixture-doc` and commit the result.

A published personality never changes. A console has the channel layout
physically programmed into its show file, so a new control is a new version
rather than a changed one, and a server refuses a patch naming a version it
does not have rather than serving the nearest one.

The server receives only. It answers no ArtPoll, appears in no console's node
list, and transmits nothing.

A patched fixture owns every channel of its personality, including the ones
nobody moved: faders left at zero are values, not silence. Park the patch entry
to hand a parameter back to the client.

The **Home** column is what to put in a desk's home or default preset. It is
not something this server substitutes for a zero it received — zero is a real
value and stays one — it is what a freshly patched fixture should be sent so
that a layer is visible, its normal size, upright and on black rather than a
wall of zeros.

## OVS Master — `ovs.master` v1

Footprint: **6 channels**. Patch to: **the show master (one per show)**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Master Dimmer | `master_dimmer` | 0 at 0, 1 at 255 | 255 |
| 2 | Freeze Output | `master_freeze` | on at 128 and above | 0 |
| 3 | BPM Tap | `master_tap_tempo` | fires once when it reaches 128; held is not repeated | 0 |
| 4 | BPM Sync | `master_reset_phase` | fires once when it reaches 128; held is not repeated | 0 |
| 5 | BPM Scale | `master_speed_scale` | 6 steps, listed below | 128 |
| 6 | Crossfade A/B | `master_crossfader` | 0 at 0, 1 at 255 | 0 |

### BPM Scale (channel 5)

| DMX | Step |
|---|---|
| 0–42 | `0.125` |
| 43–85 | `0.25` |
| 86–127 | `0.5` |
| 128–170 | `1.0` |
| 171–213 | `2.0` |
| 214–255 | `4.0` |

## OVS Layer — `ovs.layer` v1

Footprint: **12 channels**. Patch to: **one layer**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Layer Dimmer | `layer_opacity` | 0 at 0, 1 at 255 | 255 |
| 2 | Slot Playback | `layer_slot` | 85 steps, listed below | 0 |
| 3–4 | Transform Position X | `layer_position_x` | -2 at 0, 2 at 65535 | 32768 |
| 5–6 | Transform Position Y | `layer_position_y` | -2 at 0, 2 at 65535 | 32768 |
| 7–8 | Transform Rotation Z | `layer_rotation` | -180 at 0, 180 at 65535 | 32768 |
| 9–10 | Transform Scale X | `layer_scale_x` | 0 at 0, 10 at 65535 | 6554 |
| 11–12 | Transform Scale Y | `layer_scale_y` | 0 at 0, 10 at 65535 | 6554 |

### Slot Playback (channel 2)

| DMX | Step |
|---|---|
| 0–3 | `black` |
| 4–6 | `1` |
| 7–9 | `2` |
| 10–12 | `3` |
| 13–15 | `4` |
| 16–18 | `5` |
| 19–21 | `6` |
| 22–24 | `7` |
| 25–27 | `8` |
| 28–30 | `9` |
| 31–33 | `10` |
| 34–36 | `11` |
| 37–39 | `12` |
| 40–42 | `13` |
| 43–45 | `14` |
| 46–48 | `15` |
| 49–51 | `16` |
| 52–54 | `17` |
| 55–57 | `18` |
| 58–60 | `19` |
| 61–63 | `20` |
| 64–66 | `21` |
| 67–69 | `22` |
| 70–72 | `23` |
| 73–75 | `24` |
| 76–78 | `25` |
| 79–81 | `26` |
| 82–84 | `27` |
| 85–87 | `28` |
| 88–90 | `29` |
| 91–93 | `30` |
| 94–96 | `31` |
| 97–99 | `32` |
| 100–102 | `33` |
| 103–105 | `34` |
| 106–108 | `35` |
| 109–111 | `36` |
| 112–114 | `37` |
| 115–117 | `38` |
| 118–120 | `39` |
| 121–123 | `40` |
| 124–126 | `41` |
| 127–129 | `42` |
| 130–132 | `43` |
| 133–135 | `44` |
| 136–138 | `45` |
| 139–141 | `46` |
| 142–144 | `47` |
| 145–147 | `48` |
| 148–150 | `49` |
| 151–153 | `50` |
| 154–156 | `51` |
| 157–159 | `52` |
| 160–162 | `53` |
| 163–165 | `54` |
| 166–168 | `55` |
| 169–171 | `56` |
| 172–174 | `57` |
| 175–177 | `58` |
| 178–180 | `59` |
| 181–183 | `60` |
| 184–186 | `61` |
| 187–189 | `62` |
| 190–192 | `63` |
| 193–195 | `64` |
| 196–198 | `65` |
| 199–201 | `66` |
| 202–204 | `67` |
| 205–207 | `68` |
| 208–210 | `69` |
| 211–213 | `70` |
| 214–216 | `71` |
| 217–219 | `72` |
| 220–222 | `73` |
| 223–225 | `74` |
| 226–228 | `75` |
| 229–231 | `76` |
| 232–234 | `77` |
| 235–237 | `78` |
| 238–240 | `79` |
| 241–243 | `80` |
| 244–246 | `81` |
| 247–249 | `82` |
| 250–252 | `83` |
| 253–255 | `84` |

## Colour Correct — `ovs.colour_correct` v1

Footprint: **8 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Brightness | `brightness` | 0 at 0, 4 at 65535 | 16384 |
| 3–4 | Contrast | `contrast` | 0 at 0, 4 at 65535 | 16384 |
| 5–6 | Saturation | `saturation` | 0 at 0, 4 at 65535 | 16384 |
| 7–8 | Gamma | `gamma` | 0.1 at 0, 4 at 65535 | 15123 |

## Colour Correct — `ovs.colour_correct` v2

Footprint: **12 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Brightness | `brightness` | 0 at 0, 4 at 65535 | 16384 |
| 3–4 | Contrast | `contrast` | 0 at 0, 4 at 65535 | 16384 |
| 5–6 | Saturation | `saturation` | 0 at 0, 4 at 65535 | 16384 |
| 7–8 | Gamma | `gamma` | 0.1 at 0, 4 at 65535 | 15123 |
| 9–10 | Temperature | `temperature_k` | 2000 at 0, 10000 at 65535 | 36863 |
| 11–12 | Tint | `tint` | -1 at 0, 1 at 65535 | 32768 |

## Tint — `ovs.tint` v1

Footprint: **7 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Colour Red | `colour` | 0 at 0, 1 at 255 | 255 |
| 2 | Colour Green | `colour` | 0 at 0, 1 at 255 | 255 |
| 3 | Colour Blue | `colour` | 0 at 0, 1 at 255 | 255 |
| 4 | Colour Alpha | `colour` | 0 at 0, 1 at 255 | 255 |
| 5–6 | Amount | `amount` | 0 at 0, 1 at 65535 | 0 |
| 7 | Keep brightness | `preserve_luma` | on at 128 and above | 255 |

## Pixelate — `ovs.pixelate` v1

Footprint: **2 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Block size | `block_px` | 1 at 0, 256 at 65535 | 0 |

## Blur — `ovs.blur` v1

Footprint: **4 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Radius | `radius_px` | 0 at 0, 64 at 65535 | 0 |
| 3–4 | Amount | `amount` | 0 at 0, 1 at 65535 | 65535 |

## Crop — `ovs.crop` v1

Footprint: **8 channels**. Patch to: **one effect instance in a chain**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Left | `left_px` | 0 at 0, 8192 at 65535 | 0 |
| 3–4 | Right | `right_px` | 0 at 0, 8192 at 65535 | 0 |
| 5–6 | Top | `top_px` | 0 at 0, 8192 at 65535 | 0 |
| 7–8 | Bottom | `bottom_px` | 0 at 0, 8192 at 65535 | 0 |

## Solid Colour — `ovs.solid_colour` v1

Footprint: **4 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Colour Red | `colour` | 0 at 0, 1 at 255 | 255 |
| 2 | Colour Green | `colour` | 0 at 0, 1 at 255 | 255 |
| 3 | Colour Blue | `colour` | 0 at 0, 1 at 255 | 255 |
| 4 | Colour Alpha | `colour` | 0 at 0, 1 at 255 | 255 |

## Test Pattern — `ovs.test_pattern` v1

Footprint: **2 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Grid size | `grid_px` | 8 at 0, 512 at 255 | 28 |
| 2 | Colour bars | `bars` | on at 128 and above | 255 |

## Strobe — `ovs.strobe` v1

Footprint: **5 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Colour Red | `colour` | 0 at 0, 1 at 255 | 255 |
| 2 | Colour Green | `colour` | 0 at 0, 1 at 255 | 255 |
| 3 | Colour Blue | `colour` | 0 at 0, 1 at 255 | 255 |
| 4 | Colour Alpha | `colour` | 0 at 0, 1 at 255 | 255 |
| 5 | Rate | `hz` | 0.5 at 0, 30 at 255 | 82 |

## Gradient — `ovs.gradient` v1

Footprint: **10 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | From Red | `colour_a` | 0 at 0, 1 at 255 | 255 |
| 2 | From Green | `colour_a` | 0 at 0, 1 at 255 | 255 |
| 3 | From Blue | `colour_a` | 0 at 0, 1 at 255 | 255 |
| 4 | From Alpha | `colour_a` | 0 at 0, 1 at 255 | 255 |
| 5 | To Red | `colour_b` | 0 at 0, 1 at 255 | 0 |
| 6 | To Green | `colour_b` | 0 at 0, 1 at 255 | 0 |
| 7 | To Blue | `colour_b` | 0 at 0, 1 at 255 | 0 |
| 8 | To Alpha | `colour_b` | 0 at 0, 1 at 255 | 255 |
| 9–10 | Angle | `angle_deg` | 0 at 0, 360 at 65535 | 16384 |

## Grid — `ovs.grid` v1

Footprint: **11 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Spacing | `spacing_px` | 4 at 0, 512 at 65535 | 7740 |
| 3 | Line width | `line_px` | 1 at 0, 64 at 255 | 4 |
| 4 | Line colour Red | `line_colour` | 0 at 0, 1 at 255 | 255 |
| 5 | Line colour Green | `line_colour` | 0 at 0, 1 at 255 | 255 |
| 6 | Line colour Blue | `line_colour` | 0 at 0, 1 at 255 | 255 |
| 7 | Line colour Alpha | `line_colour` | 0 at 0, 1 at 255 | 255 |
| 8 | Background Red | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 9 | Background Green | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 10 | Background Blue | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 11 | Background Alpha | `background_colour` | 0 at 0, 1 at 255 | 255 |

## Shape — `ovs.shape` v1

Footprint: **15 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Shape | `shape` | 3 steps, listed below | 42 |
| 2–3 | Width | `width_px` | 0 at 0, 8192 at 65535 | 2048 |
| 4–5 | Height | `height_px` | 0 at 0, 8192 at 65535 | 2048 |
| 6–7 | Rotation | `rotation_deg` | 0 at 0, 360 at 65535 | 0 |
| 8 | Colour Red | `colour` | 0 at 0, 1 at 255 | 255 |
| 9 | Colour Green | `colour` | 0 at 0, 1 at 255 | 255 |
| 10 | Colour Blue | `colour` | 0 at 0, 1 at 255 | 255 |
| 11 | Colour Alpha | `colour` | 0 at 0, 1 at 255 | 255 |
| 12 | Background Red | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 13 | Background Green | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 14 | Background Blue | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 15 | Background Alpha | `background_colour` | 0 at 0, 1 at 255 | 0 |

### Shape (channel 1)

| DMX | Step |
|---|---|
| 0–85 | `ellipse` |
| 86–170 | `rectangle` |
| 171–255 | `triangle` |

## Noise — `ovs.noise` v1

Footprint: **5 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Scale | `scale_px` | 2 at 0, 512 at 65535 | 3855 |
| 3 | Coloured | `coloured` | on at 128 and above | 0 |
| 4–5 | Amount | `amount` | 0 at 0, 1 at 65535 | 65535 |

## Clock — `ovs.clock` v1

Footprint: **8 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Text Red | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 2 | Text Green | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 3 | Text Blue | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 4 | Text Alpha | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 5 | Background Red | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 6 | Background Green | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 7 | Background Blue | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 8 | Background Alpha | `background_colour` | 0 at 0, 1 at 255 | 255 |

## Countdown — `ovs.countdown` v1

Footprint: **8 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1 | Text Red | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 2 | Text Green | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 3 | Text Blue | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 4 | Text Alpha | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 5 | Background Red | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 6 | Background Green | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 7 | Background Blue | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 8 | Background Alpha | `background_colour` | 0 at 0, 1 at 255 | 255 |

## Text — `ovs.text` v1

Footprint: **15 channels**. Patch to: **one generator instance in a slot**.

| Channel | Name | Control | Values | Home |
|---|---|---|---|---|
| 1–2 | Cap height | `cap_px` | 4 at 0, 1024 at 65535 | 6682 |
| 3–4 | Stroke weight | `weight` | 0.02 at 0, 0.4 at 65535 | 17246 |
| 5–6 | Tracking | `tracking` | -0.2 at 0, 1 at 65535 | 17476 |
| 7 | Alignment | `align` | 3 steps, listed below | 42 |
| 8 | Text Red | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 9 | Text Green | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 10 | Text Blue | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 11 | Text Alpha | `text_colour` | 0 at 0, 1 at 255 | 255 |
| 12 | Background Red | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 13 | Background Green | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 14 | Background Blue | `background_colour` | 0 at 0, 1 at 255 | 0 |
| 15 | Background Alpha | `background_colour` | 0 at 0, 1 at 255 | 0 |

### Alignment (channel 7)

| DMX | Step |
|---|---|
| 0–85 | `centre` |
| 86–170 | `left` |
| 171–255 | `right` |

