# OpenVideoServer — Generators and Effects

Generated from the published manifests by `cargo run -p ovs-proto --bin ovs-module-doc`.
Do not edit by hand: a printed list that disagrees with the machine is worse than none,
because it is believed.

A manifest is published once and never edited. Adding a parameter is a new version, so a
show saved last year opens looking the way it was left.

Every published version is listed here, which is not what a slot offers: a source list
shows the newest of each type, and an older one is kept so that a show naming it still
opens. Two entries with the same name are that, rather than a mistake.

## Generators

A generator makes a picture without a file. It implements the same source contract a clip
does, so a layer plays one exactly as it plays a clip and nothing above it knows the
difference.

### Solid Colour — `ovs.solid_colour` v1

Fills the layer with one colour.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Colour | `colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |

### Test Pattern — `ovs.test_pattern` v2

The service pattern: bars, a drift, a sweep and a caption.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Caption | `caption` | text, up to 32 characters | `COMPOSITION` |

### Strobe — `ovs.strobe` v1

Flashes one colour at a rate.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Colour | `colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Rate | `hz` | a number from 0.5 to 30 | 10 |

### Strobe — `ovs.strobe` v2

Flashes one colour at a rate, over the picture or against it.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Colour | `colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Rate | `hz` | a number from 0.5 to 30 | 10 |
| Mode | `mode` | one of `colour` (Colour), `invert` (Invert) | `colour` |

### Gradient — `ovs.gradient` v1

A linear gradient between two colours, at an angle.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| From | `colour_a` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| To | `colour_b` | a colour, linear RGBA | 0.00, 0.00, 0.00, 1.00 |
| Angle | `angle_deg` | a number from 0 to 360 | 90 |

### Grid — `ovs.grid` v1

Lines at a spacing, in one colour on another.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Spacing | `spacing_px` | a number from 4 to 512 | 64 |
| Line width | `line_px` | a number from 1 to 64 | 2 |
| Line colour | `line_colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Background | `background_colour` | a colour, linear RGBA | 0.00, 0.00, 0.00, 1.00 |

### Shape — `ovs.shape` v1

One shape, centred, in a colour on a background.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Shape | `shape` | one of `ellipse` (Ellipse), `rectangle` (Rectangle), `triangle` (Triangle) | `ellipse` |
| Width | `width_px` | a number from 0 to 8192 | 256 |
| Height | `height_px` | a number from 0 to 8192 | 256 |
| Rotation | `rotation_deg` | a number from 0 to 360 | 0 |
| Colour | `colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Background | `background_colour` | a colour, linear RGBA | 0.00, 0.00, 0.00, 0.00 |

### Noise — `ovs.noise` v1

Value noise at a scale, monochrome or coloured.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Scale | `scale_px` | a number from 2 to 512 | 32 |
| Coloured | `coloured` | on or off | off |
| Amount | `amount` | a number from 0 to 1 | 1 |

### Clock — `ovs.clock` v1

The time of day, drawn large.

**Retired.** Not offered when adding one, and still served to a show that already
             names it, so nothing that was saved with it loses a layer.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| 24-hour | `hour_24` | on or off | on |
| Seconds | `show_seconds` | on or off | on |
| Text | `text_colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Background | `background_colour` | a colour, linear RGBA | 0.00, 0.00, 0.00, 1.00 |
| UTC offset | `utc_offset_minutes` | a number from -720 to 840 | 0 |

### Countdown — `ovs.countdown` v1

Time remaining, counting down to zero.

**Retired.** Not offered when adding one, and still served to a show that already
             names it, so nothing that was saved with it loses a layer.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Duration | `duration_s` | a number from 1 to 86399 | 600 |
| Start on trigger | `from_trigger` | on or off | on |
| Count up past zero | `count_past_zero` | on or off | off |
| Text | `text_colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Background | `background_colour` | a colour, linear RGBA | 0.00, 0.00, 0.00, 1.00 |

### Text — `ovs.text` v1

Words, set large and centred.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Text | `text` | text, up to 240 characters | `OPENVIDEOSERVER` |
| Cap height | `cap_px` | a number from 4 to 1024 | 108 |
| Stroke weight | `weight` | a number from 0.02 to 0.4 | 0.12 |
| Tracking | `tracking` | a number from -0.2 to 1 | 0.12 |
| Alignment | `align` | one of `centre` (Centre), `left` (Left), `right` (Right) | `centre` |
| Text | `text_colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Background | `background_colour` | a colour, linear RGBA | 0.00, 0.00, 0.00, 0.00 |

## Effects

An effect takes a picture and gives one back, and what comes out is drawn where what
went in would have been.

**A chain on a layer is what this build draws.** The model also holds a chain on a slot,
meant to run on the source before the layer's transform; nothing draws that yet, so an
effect put there is saved and does nothing.

Every effect is neutral at its published defaults: adding one to a chain and touching
nothing cannot alter a show that was already right.

### Colour Correct — `ovs.colour_correct` v1

Brightness, contrast, saturation and gamma.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Brightness | `brightness` | a number from 0 to 4 | 1 |
| Contrast | `contrast` | a number from 0 to 4 | 1 |
| Saturation | `saturation` | a number from 0 to 4 | 1 |
| Gamma | `gamma` | a number from 0.1 to 4 | 1 |

### Colour Correct — `ovs.colour_correct` v2

Brightness, contrast, saturation, gamma, and the white point.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Brightness | `brightness` | a number from 0 to 4 | 1 |
| Contrast | `contrast` | a number from 0 to 4 | 1 |
| Saturation | `saturation` | a number from 0 to 4 | 1 |
| Gamma | `gamma` | a number from 0.1 to 4 | 1 |
| Temperature | `temperature_k` | a number from 2000 to 10000 | 6500 |
| Tint | `tint` | a number from -1 to 1 | 0 |

### Tint — `ovs.tint` v1

Mixes the picture towards one colour.

**Retired.** Not offered when adding one, and still served to a show that already
             names it, so nothing that was saved with it loses a layer.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Colour | `colour` | a colour, linear RGBA | 1.00, 1.00, 1.00, 1.00 |
| Amount | `amount` | a number from 0 to 1 | 0 |
| Keep brightness | `preserve_luma` | on or off | on |

### Pixelate — `ovs.pixelate` v1

Averages the picture into square blocks.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Block size | `block_px` | a number from 1 to 256 | 1 |

### Blur — `ovs.blur` v1

Softens the picture, horizontally and vertically.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Radius | `radius_px` | a number from 0 to 64 | 0 |
| Amount | `amount` | a number from 0 to 1 | 1 |

### Crop — `ovs.crop` v1

Cuts a number of pixels off each edge.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Left | `left_px` | a number from 0 to 8192 | 0 |
| Right | `right_px` | a number from 0 to 8192 | 0 |
| Top | `top_px` | a number from 0 to 8192 | 0 |
| Bottom | `bottom_px` | a number from 0 to 8192 | 0 |

### Mirror — `ovs.mirror` v1

Folds one half of the picture onto the other.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Horizontal | `horizontal` | one of `off` (Off), `left` (Keep left), `right` (Keep right) | `off` |
| Vertical | `vertical` | one of `off` (Off), `top` (Keep top), `bottom` (Keep bottom) | `off` |

### Colorize — `ovs.colorize` v1

Keeps the brightness and paints one colour over it.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Hue | `hue` | a number from 0 to 360 | 0 |
| Saturation | `saturation` | a number from 0 to 1 | 1 |
| Amount | `amount` | a number from 0 to 1 | 0 |

### RGB Correct — `ovs.rgb_correct` v1

A black level and a white level for each channel.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Red black | `lift_r` | a number from -1 to 1 | 0 |
| Green black | `lift_g` | a number from -1 to 1 | 0 |
| Blue black | `lift_b` | a number from -1 to 1 | 0 |
| Red white | `gain_r` | a number from 0 to 4 | 1 |
| Green white | `gain_g` | a number from 0 to 4 | 1 |
| Blue white | `gain_b` | a number from 0 to 4 | 1 |

### RGB Split — `ovs.rgb_split` v1

Offsets the red and blue channels in opposite directions.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Offset | `offset_px` | a number from 0 to 256 | 0 |
| Angle | `angle_deg` | a number from 0 to 360 | 0 |

### TV Glitch — `ovs.tv_glitch` v1

Line jumps, snow and a rolling error, under one master.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Amount | `amount` | a number from 0 to 1 | 0 |
| Line jumps | `tear` | a number from 0 to 1 | 0.5 |
| Snow | `noise` | a number from 0 to 1 | 0.5 |
| Rolling error | `roll` | a number from 0 to 1 | 0.5 |

### TV Glitch — `ovs.tv_glitch` v2

Line jumps, snow, a rolling error and a lost vertical hold.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Amount | `amount` | a number from 0 to 1 | 0 |
| Line jumps | `tear` | a number from 0 to 1 | 0.5 |
| Snow | `noise` | a number from 0 to 1 | 0.5 |
| Rolling error | `roll` | a number from 0 to 1 | 0.5 |
| Out of sync | `vertical_hold` | a number from 0 to 1 | 0.5 |

### Mask — `ovs.mask` v1

A picture from the library, multiplied in or cut out.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Picture | `image` | a picture from the library | nothing |
| Mode | `mode` | one of `multiply` (Multiply), `cut_out` (Cut out) | `multiply` |
| Invert | `invert` | on or off | off |
| Amount | `amount` | a number from 0 to 1 | 1 |

### Invert — `ovs.invert` v1

Turns every colour into its opposite.

| Parameter | Key | Takes | Default |
|---|---|---|---|
| Inverted | `on` | on or off | off |

