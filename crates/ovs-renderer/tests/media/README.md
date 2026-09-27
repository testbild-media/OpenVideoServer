# The clips the tests decode

Four files, all the same thirty frames: ten red, ten green, ten blue, 64x64 at
25 fps — small enough to commit and obvious enough that a wrong frame is visible
in one `assert`. The renderer's own tests decode them, and so do `ovs-core`'s
ingest tests, on purpose: if the reading the server takes of a file ever
disagrees with what the renderer plays, the library shows an operator one thing
and the wall shows another.

| File | Codec | Ingest | Why it is here |
|---|---|---|---|
| `colours-hevc.mp4` | HEVC 8-bit, 1s GOP | **accepted** | What an OVS asset is. Every clause of the playback contract met. |
| `colours-hap.mov` | HAP Alpha, Snappy | **accepted** | The other half of the contract: a texture, not a coded picture. |
| `colours.mov` | ProRes 422 Proxy | refused | All-intra, and `yuv422p10le` — the ten-bit path's own fixture. |
| `colours-h264.mp4` | H.264, 15-frame GOP | refused | Long-GOP. A seek lands on a keyframe and runs forward from there. |

The last two are refused by ingest since the owner narrowed the policy on
2026-09-23 — HEVC 8-bit, HEVC 10-bit and HAP reach the library and nothing else
does. They stay committed because the **renderer** still has to decode whatever
is already in a library, and because a policy needs something real to refuse:
`a_clip_outside_the_profile_is_refused_with_the_list_of_what_is_wrong` uploads
`colours.mov` and reads the refusal.

`colours.mov` earns its place twice over: ProRes 422 Proxy decodes to
`yuv422p10le`, so it is the committed fixture the native ten-bit path is tested
against.

All four come from one source:

```bash
# The original, which the other three are made from.
ffmpeg -i colours.mov -c:v libx264 -preset veryslow -crf 30 -pix_fmt yuv420p \
  -g 15 -r 25 colours-h264.mp4

# An OVS asset: one-second keyframes, no B-frames, every colour tag written.
ffmpeg -i colours.mov -an -c:v libx265 -pix_fmt yuv420p -preset medium \
  -x265-params "log-level=none:keyint=25:min-keyint=25:scenecut=0:bframes=0:\
colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited" \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 \
  -tag:v hvc1 colours-hevc.mp4

ffmpeg -i colours.mov -an -c:v hap -format hap_alpha -compressor snappy \
  colours-hap.mov
```

The `x265-params` matter: the `-color_*` flags alone tag the container, and the
probe reads what the *bitstream* says. Without `colorprim` and `transfer` in the
encoder's own parameters the file arrives untagged and the contract refuses it —
which is the contract working, and a good half hour to discover.

Check any of them with:

```bash
cargo run -p ovs-media --example profile-probe -- colours-hevc.mp4
```
