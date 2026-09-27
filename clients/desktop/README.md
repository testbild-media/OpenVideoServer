# OpenVideoServer desktop client

Electron and TypeScript. The show never depends on this
process: it connects to a server that is already running one, and closing it
changes nothing on stage.

## Running it

```bash
npm install                 # from the repository root, once
npm run client:build        # bundles main, preload and the page into dist/
npm start --workspace @ovs/desktop
```

The first screen asks for a server address, an operator name and a password. A
bare address gets the port `ovsd` listens on by default (`7420`); a full URL is
taken as typed. A server that has no operator account yet is in first-run setup,
and the first credentials entered are the ones it is set up with.

## What it brings with it

`npm run client:build` copies `ffmpeg` and `ffprobe` into `dist/bin/`, so a file
can be read, and converted where it has to be, without anybody installing a
video toolkit first. The client prefers them over whatever is on the path: behaving
one way on one machine and another elsewhere, depending on what somebody
installed for an unrelated reason, is worse than behaving the same way
everywhere. `OVS_FFMPEG` and `OVS_FFPROBE` override both, for a machine with a
build worth preferring.

They are build-time dependencies. Nothing the finished client loads comes out of
`node_modules`: the two programs are assets, copied the way the page's HTML is,
because an executable cannot be bundled into a JavaScript file.

Packaging has to leave `dist/bin` outside the asar archive, since an executable
inside one cannot be run. The client looks in both places, so it works either
way.

The builds available on npm are GPL-3.0-or-later. That is
owner decision Q18, and it wants settling before the client is handed
to anybody outside this show.

## Shape

```text
src/shared/    the reasoning: mirror, connection, HTTP client, view, live,
               work, clock, editor, mapping, patch, media, ingest
src/main/      Electron's main process: the session, the window, IPC, and the
               only half of this client with a filesystem
src/preload.ts the bridge, which is a list of named commands
src/ui/        the page
```

Everything that reaches a server runs in the main process, including the session
token. The window is sandboxed, context-isolated, cannot navigate, and its
content-security policy gives it no network of its own beyond one socket scheme;
it asks the main process for named commands and draws what it is sent. It holds
no show state: a pad lights up because the server said the slot is active, never
because it was clicked. The one thing it reaches a server for itself is the
preview stream, because only a page can decode H.264 — and what it holds to do
that is a ticket for one surface rather than the session, which is under "The
preview stream" below.

`src/shared/` is plain TypeScript with the socket, the clock and `fetch` handed
in, which is why a gap in the update stream, a refused handshake and a half-open
connection are tested rather than assumed.

## Live Mode

three bands: the global area, the dominant layer × slot matrix,
and the preview/work area under it. The master clock sits in the top bar with
TAP and RESET beside it, where the drawing puts it — there is exactly one of it
in the whole system, so it is not the live screen's to hide when
another mode is on.

`src/shared/live.ts` is the reasoning, with no window and no network, and
`src/ui/live.ts` draws it. Four things it is careful about.

- **Live control is not editing.** The whole of what this screen can send is
  `LiveCommand` — select a slot, change a layer's live state, change the master,
  move an effect's live values or put them back, tap, reset the phase — and
  every one of those is state that sits on top of the show and is never
  written into it. `isLiveCommand` checks it again at run time on the
  one path that reaches a server. There is no control here for layer order,
  blend mode, slot assignment or any `default_*` value, because those are the
  show document and the document is edited in EDIT. The
  reference drawing has arrows beside each layer for moving it up and down the
  composite; they are deliberately absent, because pressing one would be an edit
  made from the live screen.
- **A control that has been moved is not a control that has landed.** A fader
  moved here is a request until the server answers it, so it is drawn as
  pending — its own colour, neither "done" nor "broken" — and it clears on the
  answer to its own request rather than on the update stream. That distinction
  is the whole of it. A live endpoint answers 204 both when it accepted the
  change and when the change moved nothing — a value the show was already at, a
  fader held still, a bus a layer is already on — and it publishes an update
  only when something actually moved. A control that waited for the update would
  therefore hang for ever on exactly the requests an operator makes most, since
  dragging a fader sends every position it passes through. So the answer is what
  clears a control, and the update stream is what *corrects* it: a value the
  server clamped, or a console holding the same fader, arrives as an update and
  is then what the control shows. A refusal stays on screen, in the server's own
  words, until the control is moved again. A client that has lost its socket
  leaves every request pending, which is exactly what has happened to them.
  Nothing is queued, retried or resent: a client that queued live commands would
  go on moving a show that had carried on without it.
- **Never a number the server did not send.** A value the mirror has not
  delivered reads as an em dash. The tempo field is empty rather than zero, the
  layer rate reads `—` rather than `0.0`, and the faders sit where they are
  rather than dropping to their minimum — a dimmer drawn at 0% is a claim about
  a stage that may be anything at all. The one piece of arithmetic on the screen
  is the show's tempo, which is `MasterClock::beats_per_second` in beats per
  minute — the master's tempo times the master's scale, with nothing per
  layer in it, because a layer has no rate of its own (owner, 2026-09-23).
  It is `—` when the server has not sent the master clock. What a layer
  shows beside it is the *active slot's* sync setting: FREE, or the beat
  count the slot is locked to.
- **The live value and the show's are both shown, and marked.** `route` and
  `opacity` exist twice: once as what the layer is doing and once as what the
  show opens it on. Both travel in the
  view, the live one is what the control shows, and a live value that is not the
  show's is marked and says which is which. A layer's `enabled` is reported
  and not offered, because there is no live enable and no endpoint that would
  change one.

Slot 0 is on every bank rather than only the first: it is the pad an operator
reaches for when something is wrong, it switches immediately whatever the grid
says (`select_slot_at`), and a black button on another page is a black button
that is not there. Banks are derived from what the show has slots for rather
than fixed at the drawing's seven, because a bank of pads nothing is assigned to
is a page that cannot do anything.

A tap answers with the tempo it produced and whether it produced one. The first
tap of a run never changes anything — one tap is not an interval — so the screen
says "tapping…" until `TapTempoResponse.changed` turns true, rather than showing
the old tempo as though it were new.

### The beat strip

The strip the drawing puts under the BPM: one mark per beat of the bar, the beat
the server reports lit, and the phase running through it. It exists now because
the server reports where its clock is — `ClockTelemetry` carries `bar`,
`beat_in_bar`, `phase`, `beats_per_bar` and `beats_per_second` with the rest of
the telemetry, and it is the one thing on this screen that moves.

`src/shared/clock.ts` is the reasoning and `src/ui/live.ts` draws it.

- **It is a prediction, and it says so.** Telemetry is a periodic sample, about
  one a second; between samples the strip runs on from the last one at the
  `beats_per_second` it reported, and each new sample replaces the prediction
  rather than nudging it. A strip that only moved when telemetry arrived would
  stutter, and one that never corrected would drift away from the show. Nothing
  accumulates, so an error in one prediction cannot become drift in the next.
  The rate arrives already multiplied — tempo times master speed scale — because
  the two numbers it comes from travel in different messages and a screen that
  multiplied the wrong pair would be wrong in a way nobody could see.
- **No clock, no strip.** Telemetry carries no `clock` at all while no show is
  open, and that reads as unavailable — an em dash, as everywhere else in this
  client — never as a beat at zero, which is a real position a running clock
  passes through every bar. A sample that is not a beat of a bar (a bar of no
  beats, a beat outside its own bar, a rate that is not a number) is refused for
  the same reason.
- **A prediction nobody has corrected stops being one.** After five seconds with
  nothing from the server the strip says it does not know rather than beating on
  by itself, and it drops what it was predicting from the moment the connection
  goes: the show is carrying on without this client and only the server can say
  where its beat is.
- **It costs nothing when nobody is looking.** The frame loop runs only while
  Live Mode is the mode on screen, the window is visible and there is a sample to
  predict from; the strip is hidden with the mode rather than frozen in it. A
  frame writes one transform on one mark. Where the machine asks for reduced
  motion it lights the beat and does not run the phase.

The tempo beside it stays in the top bar in every mode, because it is state; the
beat does not, because it is a prediction that needs a server still speaking to
stay true.

### The playhead bar

Where the clip on a playing layer has got to, in the layer status band under
what that layer is showing: a bar and the position and length beside it. It is
the thing an operator cueing the next clip is actually asking for, and it exists
now because the server reports it — `ServerTelemetry.playheads` carries one
`LayerPlayhead` per layer that is playing a clip.

`src/shared/playhead.ts` is the reasoning and `src/ui/live.ts` draws it. It is
the beat strip's arithmetic again, on the same frame loop, with two rules the
beat does not have.

- **A layer nobody reported a playhead for has no bar.** Not an empty track and
  not one at zero: a generator layer draws itself and has no timeline to be a
  position in, a black layer has no clip, and a clip that played once and
  blacked has finished. All three are absent from the telemetry, and "at the
  beginning" is a different picture from "there is no clip". The set of layers
  that have a bar is replaced by each sample rather than merged into, so a layer
  the server has stopped reporting loses its bar rather than keeping the last
  one.
- **A sample belongs to the clip it was taken on.** A sample can be most of a
  second old and a pad hit is instant, so a sample whose `slot` or
  `media_item_id` is no longer what the layer is playing is thrown away rather
  than drawn over the new clip. That is what those two fields are on the wire
  for.
- **It is a prediction, at the rate the server sent.** `speed_ms_per_second` is
  the whole rate already multiplied out — the slot's own rate, its pass, the
  master speed and the tempo — because the parts travel in different messages.
  It is signed, so a reversed slot and the return leg of a bounce run the bar
  backwards, and it is zero for a frozen clip, which stops it: a bar animating over a held frame would be an animation of something
  that is not moving. Neither is hidden; a held bar is dimmed and says so, because
  a bar that merely stood still would read as a client that had stopped hearing.
- **It never leaves the clip.** What happens at the end is the slot's — loop,
  freeze, black or bounce — and only the server knows when it happened, so the bar
  waits at the end (or at the start, running backwards) for the next sample
  rather than guessing which of the four it was.
- **Seconds, not frames.** The sample carries both and neither is derivable from
  the other, which is why both are sent; what an operator reads while a hand
  hovers over the next pad is eight of twelve. The frame the server last reported
  is in the tooltip, said to be the reported one rather than a predicted one,
  because a frame index worked out here would be a number no server sent.

It costs what the beat strip costs, because it is the same loop: nothing at all
unless Live Mode is on screen, the window is visible and there is something to
predict from, and a frame writes one transform per bar. Where the machine asks
for reduced motion the bars show each sample where it lands and do not run
between them.

### The contextual work area

The bottom band of the drawing, in the four parts the drawing has it in: the
preview/stream box, a rail of tabs lettered up its right-hand edge — COMPOSITION
and SLICES — and a second rail up the left-hand edge of the work panel beside it
— BROWSER, GENERATOR, OPTIONS and EFFECTS. The rails
carry their labels sideways as the drawing letters them, which is not decoration:
tabs down an edge cost the band no height, and the matrix above is the thing that
must not be squeezed.

Those names are the trap. A slice is a `Piece`, a generator's parameters are a
slot's configuration and an effect chain is a list in the document, so four of
the six are named after things that are *show configuration* — and a tab that
quietly wrote to the show would break the one rule this client is built on
. So `shared/work.ts` makes every tab declare which of three
things it is, and the screen prints it at the head of whatever the tab opens:

| tab | what it is |
| --- | --- |
| COMPOSITION | a view — the surface the server is sending, unchanged |
| SLICES | a view — the same picture with the pieces outlined over it |
| BROWSER | a view — the library, and where tonight's show uses each item |
| GENERATOR | shows, and opens EDIT — the generator slots and their parameters |
| OPTIONS | live control — the selected layer's four live values |
| EFFECTS | live control — the selected layer's effects, a control per declared parameter |

OPTIONS and EFFECTS are the two that send anything. OPTIONS sends `set_layer`:
route and opacity, which with the transform is the whole of what a layer's live
state is — a layer owns nothing about time at all. A slot's own settings are
sent with `set_slot`, which is a document edit made under a running show: how
long its pass is, how fast it runs through one, what its end does and where its
picture sits. The matrix row has the layer's in the space the drawing gives it;
OPTIONS has room for what the show opens the layer on beside each one, and for
the values that are the document — the blend mode, whether the layer is switched
on, what the end of the running clip does — shown and not offered. Both tabs have
controls in them, so both are built once and updated in place: a fader rebuilt
mid-drag is a fader taken out from under an operator's finger.

**EFFECTS is live control. GENERATOR still cannot be.** The specification lists
exposed effect parameters as live state and §11 names them
`effect.<instance>.parameter.<name>` and `generator.<instance>.parameter.<name>`
in the parameter registry, and the two are no longer in the same position.

This server publishes `PUT /api/v1/live/effects/{effect_instance_id}`, so every
parameter an effect declares is a control on the EFFECTS tab, drawn from the
catalogue's own declaration — the kind, the range and the default are the
server's, so an effect this build has never heard of arrives with working
controls. A value moved there sits *on top of* what the show configured and is
never written into it, which is why the tab shows both: what the
effect is doing now, and what the show opens it on, with the ones somebody has
moved marked. Putting an instance back sends `{"reset": true}` rather than a copy
of the configured values, because a live value that happens to equal the
configured one is still a live value, and the two would part company the moment
the show was edited under it. A refusal is shown against the control that
caused it, in the server's own words: 400 `invalid_parameter` quotes the manifest
— "radius_px must be a number between 0 and 64" — and paraphrasing it would be
this client stating a range the server never gave. Only the selected layer's
chains get controls, because a screenful of faders belonging to layers the
operator is not looking at is a screenful of ways to move the wrong thing.

Nothing publishes a route that sets a *generator's* parameter — `/live` carries
the layer, the slot, the master, the effects, the tap and the phase, and nothing
else — so GENERATOR prints that in full and shows the values as the show
configures them. A slider there would either do nothing or write into the
document and call itself live, and both are worse than saying what is true.

SLICES draws the pieces over whichever surface is being watched rather than
fetching a second picture of one: over the composition a piece is the region it
*takes*, over an output it is the region it *fills*, and those are not
interchangeable. The order, which piece wins where two meet and whether a piece
reaches the wall pixel for pixel are `shared/mapping.ts`'s own answers — the ones
the mapping editor and `OutputMapping::build` give — rather than a second
implementation of them. A surface whose size this client does not know draws
nothing and says so, because "nothing is mapped here" and "this client cannot
say" are different things and only one of them is about the show.

The way out of the configuration tabs is EDIT, opened at exactly the thing
that was pressed — including from EFFECTS, because which effects a chain has and
in what order is the document however live their parameters are. That is a request to *look*: the editor loads its own copy,
nothing on stage moves, and it refuses while it is holding work nobody has saved
and says so rather than throwing it away.

Which tab is open is the client's own state, not the show's. It survives a
reconnection untouched and restoring it sends nothing — where an operator was
looking is not a fact about a server, and a socket that has only just come back
does not need traffic to say so.

## The show editor

The window keeps one division: LIVE is the show, and EDIT — with
PATCH beside it, which is the same document seen from the console's end — is
what it was built from. Switching between them changes nothing on the wall, and
nothing in either reaches the wall until a show is saved.

A show is one document. The server hands the whole thing over, takes the
whole thing back, and refuses the save outright if any part of it breaks an
invariant — there is no per-field endpoint, so a half-applied edit cannot leave
a show internally inconsistent. `src/shared/editor.ts` is that document in
memory and every edit an operator can make to it, as plain functions with no
window and no network, which is how `test/editor.test.ts` checks them against
the rules the server applies.

Three things it is careful about.

- **Nothing is dropped.** Every edit copies along the path it touches and
  spreads the rest through, so a field this build has never heard of is still
  there, in its original place, when the document is saved. A document loaded
  and saved unchanged is byte-identical, and that is a test.
- **Nothing is invented.** The routes, blend modes, playback modes, speed
  ladder, master and composition bounds, crossfader curves and
  frame rates come from `src/shared/limits.ts`, which `test/limits.test.ts`
  reads the Rust and the generated schema to check — the same arrangement
  `versions.ts` uses, and for the same reason: the generated types carry the
  shape of the document but not its arithmetic, so an editor that guessed a
  range would offer a control the server then refuses. Every effect and
  generator parameter is drawn from the catalogue the server publishes
  (`/effects`, `/generators`) with the type, range and default it declares, so
  an effect this build has never heard of arrives with working controls. Every
  connector, mode and connector role comes from `/server/capabilities`, and
  every fixture personality, footprint and channel from `/fixtures`.
  `updated_at` after a save is the server's own number, and so is every address
  the patch page suggests.
- **Editing is not live control.** `default_opacity` and `default_transform`
  are what a layer *starts* from. What a layer is doing right now is live state,
  belongs to the console and to Live Mode, and is never written here
 . The labels say "default" for that reason.

A refused save carries the server's field-level problems, and they are put back
beside the fields they name — and on the layer row, the slot pad, the piece row
and the fixture row that contain them, because the field may be several panels
away. Every problem is also listed in full, in EDIT and in PATCH alike,
including ones in the half of the document the panel being looked at has no
control for: a refusal about a fixture is not a message somebody standing in
EDIT should have to go looking for, and one about a layer is not a message to
hide from somebody standing in PATCH.

Saving the show the server currently has open reloads it there, which is a
change to what is on the wall. The editor says so before the button is pressed,
on the patch page as well as in EDIT, and that stays true of the master, the
composition, the mapping and the patch: none of them is live control, and
nothing in either reaches a wall until the show is saved.

## The mapping editor

`Composition → Pieces → Outputs`. A piece takes a rectangle of the
composition and puts it on a rectangle of an output, so both ends are on screen
at once: the composition with every piece drawn on it, and each output with what
lands on it, in the order the renderer draws them. Rectangles can be dragged and
resized, and every one of them can also be typed — an LED wall is patched to
exact pixel boundaries, and the processor's documentation gives them in numbers.

`src/shared/mapping.ts` is the geometry, with no window and no network, and it
answers the questions the renderer asks: the draw-order sort is
`OutputMapping::build`'s, the off-screen test is `MappedPiece::is_off_screen`
written in pixels rather than in normalised coordinates, and pixel-exactness is
`Piece::is_pixel_exact`. An editor that disagreed with the renderer about which
piece is on top would be drawing a mapping the wall does not have.

- **Outputs come from the machine.** `/server/capabilities` says which sockets
  exist, what modes each offers and what each is for; an output is forced onto
  one by GPU and connector and nothing else is stored, because
  the EDID fields decide nothing and a show carrying the serial of whatever
  was in the rack that night invites somebody to match on it later. A server
  that has reported no connectors says so — that means no renderer has
  connected and said, which is not the same as a machine with no sockets.
- **A rate stays exact.** A driver reports millihertz and the model stores a
  ratio: 59 940 mHz is 60000/1001, not 59940/1000, and a scheduler given the
  second would drift. The rates the model names are matched first, by the
  arithmetic `FrameRate::millihertz` rounds them with.
- **A change to the composition says what it did.** Every piece is measured
  against that canvas, so resizing it is not a change to one thing: the pieces
  whose region the new canvas does not contain are named, and the rest are said
  to keep their rectangle and therefore take a different part of the picture.
  Nothing is moved to make it fit.
- **What the server accepts is not always what the wall wants.** Two pieces
  overlapping on one output is a legitimate mapping the renderer settles by
  draw order, so it is reported as which piece wins and across what region,
  beside a piece hanging off an edge, a piece on a socket the server calls the
  service display, and a piece being resampled rather than drawn pixel for
  pixel. Those are a different colour from a refusal on purpose: an operator
  who learns to ignore one colour will ignore the other.

## The patch page

`Personality → universe → start address`. Its own mode beside LIVE,
EDIT and MEDIA, because patching a rig is its own sitting: an operator does it
once, with the console's show file open beside them, and what they need on
screen is every fixture, every universe and every channel at once.

It is the same document. The patch lives in `Show.fixture_patch`, so the Save
on this page is the editor's Save — whole document, server validates, refusal
against the fields it names — and there is no second way to save a show.
`src/shared/patch.ts` is the reasoning, with no window and no network:
`channelRange` is `FixtureInstance::channel_range`, `collides` is
`collides_with`, and `validatePatch` is `Show::validate_patch` with the
server's own sentences in it.

- **Nothing about a personality is written in this client.** The channel count,
  the footprint, the channel names and what each channel controls come from
  `GET /fixtures`. A personality this build has never heard of is patched
  correctly, and the one it *has* heard of is still read from the server,
  because the server's copy is the one the save will be judged against. What can
  be patched follows from the same catalogue: one master, one fixture per layer,
  and an effect or generator where a module publishes a personality — which none
  does yet, and the panel says so rather than leaving a gap.
- **The address is the server's answer, not arithmetic done here.**
  `GET /shows/{id}/patch/address` knows what is already patched; this asks it
  and uses what comes back, including "no room", which is a real answer about a
  universe that has 512 channels. It also says the one thing an operator could
  be caught by: the endpoint answers from the show the server has *on disk*,
  so a fixture patched a minute ago and not yet saved is not in the answer.
- **An overlap is shown on both fixtures, before the save.** Two entries wanting
  the same channels makes the show unsaveable, and the server reports it once,
  against the earlier of the two. That is enough for a list and not enough for
  the row an operator is looking at, so both go red, both say what they meet, and
  the universe bar shows where. It is reported rather than refused because it is
  a relationship between two entries and either of them may be the one that
  should move — but a universe number the field could not carry, an address below
  one, and a footprint that runs past channel 512 are all refused at the control,
  because each is one bad number with an obvious correction.
- **Parked is not unpatched.** A disabled entry keeps its universe and address
  and holds no channels, so an operator can take a fixture out of the rig for a
  night without losing where it was, and the channels are free meanwhile. The
  picture draws it as an outline for the same reason.
- **Deleting a layer tells one story.** The show editor already removes the
  patch entries that pointed at the layer and says so; this page simply has one
  fewer row afterwards, and has no second sentence about it. A document that
  arrives with an entry pointing at a layer it does not have — which is a show
  written elsewhere — is reported in the server's words with an Unpatch beside
  it.

Editing a patch is configuration. It says which channel moves the master
dimmer; what the dimmer is doing right now is live state, belongs to the console
and to Live Mode, and nothing on this page reads or writes it.

## The media library and ingest

`Import → Processing → Uploading → Ready`. The MEDIA half of the
window is the library the server holds beside the files on their way into it.

**The library is a database view, not a filesystem browser.** Every entry is
drawn from what `/media` sent: the picture (`GET /media/{id}/thumbnail`), the
name, the state, and the dimensions, duration, rate and alpha the server
measured. No path and no storage key appears anywhere, because none is ever
sent. An item the server has no picture for — one registered before thumbnails
existed, or one nothing could be read from — answers 404 and draws its name in
the picture's place, which is what every entry was until thumbnails existed and
is not a fault.

Two things the panel says it cannot show, rather than leaving as a gap somebody
looks for: **an item's size on disk** and **the format it was prepared as**.
Both live on `MediaAsset`, which is server-internal and never serialised —
there are Rust tests that fail if either ever appears in a `MediaItem`. The
second is a product decision — the operator never chooses a codec;
the first looks like an oversight, and it is noted rather than filled in,
because a size this client invented is a number an operator might act on.

**Deleting warns before it breaks a show.** Selecting an item asks
`/media/{id}/usage`, and while a show references it the delete is not
offered at all — the shows are named instead. If a delete is refused anyway,
because a show appeared between the two requests, the refusal is put in front
of the operator with those shows named: the server's own sentence lists them
inside prose, and `usage` gives the same answer as a list.

**Ingest declares what the file is, not what it is called.** The server opens
an uploaded video and reads it, refusing one whose dimensions disagree with the
declaration and one it cannot decode at all (`check_video` in
`upload_routes.rs`), so the order matters:

```text
probe → hash → declare → send from an offset → verify
```

An operator's `.mp4` full of ProRes and their `.mov` full of a PNG are both
ordinary, and the extension decides nothing. The hash comes before the
declaration because the declaration carries it, which is what lets the server
answer "I already have that" before a byte crosses the network.

- **A queue, one file at a time.** Somebody will drop twenty files at once and
  every one of them wants the disk and the network, which a machine running a
  show has spoken for already.
- **A stopped upload carries on rather than starting again.** The endpoints take
  an offset and the server says what it holds, so a resume sends the rest. On a
  show file that is the difference between a minute and an hour. If the server
  swept the upload — nothing arrived for an hour — the upload is declared again
  *into the same media item*, because a show may already point at it.
- **No `ffprobe` is said out loud.** A machine without one cannot read a file,
  and nothing is guessed from its name: the panel says so and asks the operator
  two things — whether it is a clip or a picture, and whether it carries alpha.
  That used to be a list of internal playback formats, which meant asking
  somebody to recognise a codec; the server plays what it is given now
 , so the question got smaller.
- **Almost nothing is converted.** The queue asks the server what it can decode
  (`media_codecs` on `/server/capabilities`) and sends the file as it is when
  the answer includes it, which it usually does. A file that server cannot
  decode is prepared as ProRes first, and one neither end can read is stopped
  with both facts in the message.

`src/shared/ingest.ts` is all of that as plain logic with the disk and the
network handed in, and `src/shared/media.ts` is what an entry reads as; both are
tested without a window and without a server.

## The preview stream

`src/shared/stream.ts` watches what the renderer is drawing. The server encodes
each watched surface once and pushes every picture down a socket
(`crates/ovs-core/src/server/stream_routes.rs`, at
`/api/v1/preview/stream/<surface>`, where a surface is `composition` or an
output's identifier): sixteen bytes of header — version, a keyframe flag, the
size and a timestamp — and then one H.264 picture in Annex-B form, as one binary
message. Every picture says what size it is, so a surface that changes shape
mid-show announces it in the next picture and nothing has to be said first.

Text messages on the same socket are about the stream rather than about a
picture, one JSON object each. `{"error": "…"}` says there will be no pictures
and why; `{"encoding": {"encoder": "libx264", "hardware": false, "fps": 15}}`
says what this surface is really being encoded with. That second one exists
because a software preview is capped at fifteen frames a second so that watching
a wall cannot cost the show (owner decision Q13), and a quarter-speed picture
with no explanation is indistinguishable from a fault. The status line says it:
`1280 × 720 · 15 fps, software (libx264)`. A message this build does not
recognise is ignored rather than shown, because everything on this socket is
additive.

The module is two halves that do not touch each other. The transport is a
socket, the header and what to do when the socket drops: it reopens on a bounded
backoff, gives up a socket that has sent no picture for three seconds, waits
half a minute before asking a machine with no encoder again, and drops a message
it cannot read rather than throwing, because a newer server saying something new
must not be able to stop an operator seeing the wall. The socket and the clock
are handed in, so all of that is tested without a browser. `StreamPlayer` is the
other half: WebCodecs, a keyframe to start from, a rebuild when the surface
changes shape or the decoder gives up, and a status line — connecting, waiting
for a picture to start from, playing, the size, what it is being encoded with,
the last thing that went wrong — so the panel can say what is happening instead
of showing a black rectangle.

The panel itself is in the window, because `VideoDecoder` exists there and
nowhere else in this client. That is the one hole in the page's
content-security policy: it may open a socket, and it still cannot fetch, post
or navigate anywhere. Two consequences of putting it there:

- A server that is not this machine wants proof in the socket's first message,
  and nothing that proves anything has any business in a URL. The page holds no
  session, so the main process mints that one message, for one socket, when the
  window asks (`ovs:stream-credential`); the window sends it without reading it.

  **What it mints is a ticket, and that closes a hole this README used to
  describe.** `POST /preview/stream/{surface}/ticket` needs a session and gives
  back something far smaller than one: a permission to watch that one surface,
  good for a minute, spent the first time it is used. Until it existed the
  message was a `ClientHello` carrying the session token, which meant a window
  that had been subverted held a credential that could also load a show, move
  a fader or stop the machine — and nothing on this side could stop it sending
  that token elsewhere. It now holds a permission to watch one surface for the
  next minute, and there is nothing else it could do with it if it tried.

  One ticket per socket, never per stream. A ticket is spent by being used
  (`spend_stream_ticket`), so a second socket offering the first socket's ticket
  would be refused and the panel would reconnect for ever; and a ticket minted
  for a session that has since ended must not be sent at all.

  **And a ticket is now the only thing that socket takes.** It used to accept a
  session token as well, for a client written before tickets existed. There is
  no such client — nothing has shipped — and this is the one socket a subverted
  preview window can reach, so the wide credential had no business being
  accepted on it (owner decision Q10, closed 2026-09-22). A server that cannot
  mint a ticket is therefore one this client cannot watch, and it says so
  instead of downgrading: sending it the old handshake would put the session
  token in a window and still not get a picture.

  The alternative to all of this was relaying every picture through the main
  process, which keeps everything out of the page and puts 6 Mbit/s of encoded
  video, sixty messages a second, through the event loop that is also carrying
  the operator's commands.
- The panel can be pointed at any output. The list comes from telemetry, which
  the bridge forwards to the window (`onTelemetry`), so the chooser lists the
  outputs the server reports, named the way the telemetry names them, and stays
  hidden when the server reports none.

Only one preview runs at a time: the still pictures stand down while the live
one is painting, because both cost a machine that is running a show. The still
fallback is fetched for whichever surface the panel is pointed at
(`GET /preview/composition` or `GET /preview/{outputId}`) rather than always the
composition — a picture of one surface under a heading naming another is worse
than no picture.

## Tests

```bash
npm run client:test         # the unit suite; no server, no Electron
npm run client:typecheck
```

`test/live-mode.test.ts` is Live Mode's reasoning — the matrix, the master and
what a control says while nobody has answered it. `test/clock.test.ts` is the
beat strip: that it says nothing when the server has, that it runs at the rate
the server reported, that a fresh sample replaces a prediction that had drifted,
and that it never lights a beat the bar does not have. `test/playhead.test.ts`
is the progress bars, held to the same standard: no bar for a layer nobody
reported one for, a position predicted at the reported rate and in the reported
direction, a rate of zero that does not move, a prediction that never leaves the
clip, and a sample taken before a pad change thrown away rather than drawn over
the clip that is playing now. `test/work-area.test.ts` is the contextual tools
in the bottom band: that choosing a tab sends nothing, that the tab an operator
had open survives a reconnection without sending anything, that the tabs named
after show configuration leave the document exactly as they found it and
produce at most a request to look at it in EDIT, that an effect parameter is
live state with a route named for it and a generator's is live state with none,
that a client joining mid-show reads the overrides already in effect rather than
the values the show configured, and that the slices are read in the order the
renderer draws them. `test/live.test.ts` is a
different suite with a confusingly similar name: it runs the client's core
against a real `ovsd`, and is skipped unless one is named. `tools/client-live.sh`
is what names one — on the server machine, it builds `ovsd`, starts one of its
own on a spare port with an empty database under `mktemp -d` and no renderer,
runs this suite against it and takes the whole thing away again:

```bash
./tools/client-live.sh
```

It needs Node on that machine, which a media server has no other reason to have:
`sudo apt install --no-install-recommends nodejs npm`. Run there against a
throwaway `ovsd` on 7450, it passes six of six while the machine's own show
carries on at 59.94 on port 7420, which is the arrangement the script exists to
guarantee.

Point it at a throwaway server and never at one running a show. These tests
create a show and move the live master, which is what makes them worth having
and also what makes them destructive; the script exists so that is not a decision
anybody has to remember to make. What they cover is the contracts most likely to
drift between the Rust and the TypeScript: a show built with the editor's own
functions saved and read back whole, a document the server's validation must
refuse arriving as a refusal carrying the server's words and the path of the
field they are about, the generator, effect and personality catalogues agreeing
with `reference/MODULES.md` and `reference/FIXTURES.md` down to each parameter's kind and
default, and a run of commands whose updates the mirror applies one after another
with no gap.

### Pressing things

```bash
npm run client:press        # every control on the live screen, pressed
npm run client:overflow     # every box on every screen, measured
```

Two offscreen Electron runs against the same stub. The audit measures where
things are **drawn**; `press` measures whether they **act**, and the difference
is not academic.

**FREEZE had no click handler for two rounds of being reported.** Its listener
sat in a `#wireMaster` method that nothing ever called. The button drew, lit,
enabled and disabled itself exactly as it should, the command builder behind it
was correct and unit-tested, the server half worked — and a press did nothing.
Every check was green, because every check looked at something else: the unit
suites test the builders, the audit clicks controls only to reach a screen it
wants to measure, and the one test that presses FREEZE for real is in
`live.test.ts`, skipped unless a server is named. So `press` presses each control
and fails when nothing reaches the bridge.

It also fails on any uncaught error in the page, which is how the stub is kept
honest: the way that file rots is the bridge growing a call it does not have, and
then every control using that call dies with `is not a function`. `setSlot` had
been missing from it for some time, so every slot edit threw in both tools while
both reported green.

A control that is deliberately disabled says so and is reported as skipped. A
control that would not move anything — the bus pad a layer is already on — is not
a fair test of anything and is not one of the presses.

## The window's four quarters

LIVE is the show, EDIT is the document it was built from, PATCH is the
DMX-over-IP half of that same document, MEDIA is what the server holds and what
is on its way there. Switching between them changes nothing on the wall; nothing
in EDIT or PATCH reaches one until a show is saved, and an import is a file
arriving in a database rather than anything on stage.

## What it can do so far

Connect, sign in or set a server up, and run a show: the layer × slot matrix,
banked twelve at a time with slot 0 black on every bank and every pad showing
what it holds; per layer, what it is showing and its A/B/M bus, opacity, trigger
grid, rate against the master clock and whether the show has it switched on,
with the show's own defaults marked beside the live ones; per layer, the
previous and next slot, a retrigger and black; the master crossfader, dimmer,
speed scale and freeze; the master clock with TAP, a typed tempo, a phase reset
and the beat strip, which predicts between telemetry samples and is corrected by
each one; a progress bar on every layer that is playing a clip, with its position
and length in seconds, predicted the same way and absent altogether on a layer
the server reported no playhead for; and the preview — the live stream of the
composition or of any output the
server reports, wherever that server is, and the still picture of that same
surface when a machine will not stream at all. Every control that has been moved
and not yet confirmed by the server says so.

And read the show from the work area under it: the composition or the slices
outlined over it with what each takes and fills and which one wins where two
meet; the library with where tonight's show uses each item and what is on air;
the generator slots with every parameter as it stands, said to be
live-controllable and said to have no endpoint on this server yet; the selected
layer in full, where its four live values are changed with the show's own
beside them; and that layer's effects, where every parameter the manifest
declares is a live control with the configured value beside it, an offer to put
the instance back, and any refusal shown against the control that caused it in
the server's own words. Every one of those says which of the three things it is,
and the ones named after configuration open EDIT at the thing being looked at
rather than changing it.

Build one, too: shows listed, opened and created; the master settings —
default BPM, beats per bar, default master speed, the crossfader curve, and the
dimmer and crossfader a restart returns to; the composition canvas and its
render rate, with what a resize does to the pieces said plainly; layers added,
removed, reordered, renamed, enabled and routed, with blend mode, default
opacity and transform; the layer × slot matrix, banked twelve at a time, with
slot 0 reserved Black and every other slot taking a media item or a generator
with its parameters, a sync setting, a rate, a playback mode, a direction and a
transform; effect chains on a layer and on a slot, added from the catalogue,
reordered, switched in and out, with their parameters drawn from what the
catalogue declares; outputs built from the sockets the server reports, at the
modes those sockets offer; pieces added, copied, renamed, reordered, switched in
and out, and mapped by dragging or by typing exact pixel rectangles at both
ends; and the whole document saved, with the server's refusals shown against the
fields they name.

Patch it, too: the master fixture and a fixture per layer, from the
personalities the server publishes, at an address the server suggests; each one
moved to another universe or another address, parked without being unpatched, or
unpatched; every universe drawn as its 512 channels, with an overlap marked on
both of the fixtures that want them; and each fixture's channels listed at their
absolute addresses, which is what somebody programming a desk writes down.

Fill the library, too: files dragged in or picked, probed, hashed, declared as
what they turned out to be, uploaded a chunk at a time and verified, with a
stopped upload resumed from where the server got to rather than restarted; the
library itself listed with each item's picture, name, state and what the server
measured, what shows use each one, and delete — refused, with the shows
named, while one does.

One thing in the patch page waits on the server rather than on this client:
**an effect or generator fixture**. `FixtureTarget::Effect` takes an
`EffectInstanceId`, the panel offers any effect instance in the document whose
type and version a published personality matches, and no module publishes one —
`PersonalityTarget` has `master` and `layer` and nothing else. So the control is
there, reads the catalogue for what to offer, and finds nothing; it says so
rather than leaving a gap. A generator in a slot cannot be patched at all,
because a `SourceRef::Generator` carries a type and a version and no instance
identifier for a patch entry to point at.
