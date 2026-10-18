//! What each layer is playing, and which frame of it is on the GPU.
//!
//! This is the frame scheduler. It sits between the show state — which says
//! which slot a layer is on and when it was last triggered — and the players,
//! which turn a frame index into pixels. Everything it decides is decided
//! against the beat, because that is what a show is timed to.
//!
//! # A trigger is a counter, not a beat
//!
//! The core's trigger is a number that goes up. It has to be: a beat position
//! from the core would be on the core's timeline, and the renderer draws against
//! its own. So the scheduler watches the counter, and when it moves it stamps
//! the start of the pass on the timeline it is actually drawing against.
//!
//! # A clip's texture belongs to its layer
//!
//! Two layers playing the same file are at two different frames — the same clip
//! triggered twice is two playheads — so a playing clip's texture is keyed by
//! layer. Stills stay keyed by item and shared, because a still is the same
//! pixels for everybody and a show with one backdrop on six layers should upload
//! it once.
//!
//! # Nothing here decodes, and nothing here changes a clip over
//!
//! Opening a source reads a file and builds a decoder; letting go of one tears
//! that decoder down, which is dearer still. Neither happens on this thread —
//! both go to [`crate::media::Changeover`], which carries the measurements and
//! the reason. What happens per frame is a comparison and, when the frame index
//! has moved, one upload.
//!
//! The consequence to keep in mind while reading the trigger path: **a triggered
//! clip does not exist yet.** The layer goes on playing whatever it had, and the
//! new player is taken in on some later pass by `adopt_opened`. That is what
//! makes a trigger look instant while costing the wall nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Instant, SystemTime};

use ovs_types::config::{PlaybackDirection, PlaybackMode, Slot, SourceRef};
use ovs_types::id::{LayerId, MediaItemId};
use ovs_types::live::Transport;
use ovs_types::media::MediaKind;

use crate::backend::Compositor;
// Aliased: this module already has a Generated, which is one layer's own
// generator rather than a description of one for the card.
use crate::backend::Generated as GeneratedDraw;
use crate::media::{
    demanded_fps, pass_for, playhead_at, Changeover, FrameSource, GeneratorSource, Moment,
    NotOpened, Opened, OpeningFor, Pass, Player, ShowClock, SourceLength, SyntheticSource,
    Triggered,
};
use crate::ndi::{Ndi, NdiReceiver};
use crate::scene::TextureKey;
use crate::show_state::{MediaCatalogue, ShowState};

/// What one pass of the scheduler did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaybackReport {
    /// Layers playing a source this pass.
    pub playing: u32,
    /// Frames uploaded to the GPU this pass.
    pub uploaded: u32,
    /// Frames a player could not produce in time, since it started.
    pub late: u64,
    /// Sources that failed and left their layer on its placeholder.
    pub failed: u32,
}

/// What last drew into a layer's texture.
///
/// Three sources write `TextureKey::Layer(layer_id)` -- a generator, a network
/// receiver and a clip's player -- and each keeps its own record of what it
/// last put there, so each can decide it has nothing to do. None of the three
/// knew about the other two, and all three records are about the *source*
/// while the texture is shared between them.
///
/// The owner found it from the desk, 2026-10-10: "Slot3 NDI und Slot1+2
/// Generator", switching between slot 1 and slot 3 and back. What a generator
/// is built from includes its slot, so pendling between the same two slots
/// leaves it unchanged: the generator decided it had already drawn, drew
/// nothing, and the NDI frame that had overwritten its texture in between
/// stayed on the wall -- with the layer's effects following the show, which is
/// why the effect visibly went away. Rotating 3-2-3-1 changed the slot on
/// every switch, rebuilt every time, and looked perfectly fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    /// A generator drew it.
    Generator,
    /// A frame from a network sender.
    Ndi,
    /// A frame of a clip.
    Clip,
}

/// What a layer's generator was built from.
///
/// Everything that changes which generator this is: the slot, what it is and
/// what it was given, and how big the composition is. Not the *time*, which is
/// what the clock and the countdown vary with and which never rebuilds them.
#[derive(Debug, Clone, PartialEq)]
struct GeneratedFrom {
    slot: u16,
    source: SourceRef,
    size: ovs_types::geometry::SizePx,
    /// What a control source has done to this instance since the show
    /// opened.
    ///
    /// Part of the identity, so moving a generator's parameter from a client or
    /// a console rebuilds the source rather than leaving the wall on the value
    /// the show was built with. Rebuilding is what a redraw costs anyway: a
    /// generator draws into a buffer of its own every time its second ticks.
    overrides: ovs_types::config::ParameterMap,
}

/// What one layer's generator is being asked to draw this pass.
///
/// A struct rather than a handful of arguments because two of the seven
/// generators draw the time, and the time arrives here as two separate things:
/// the moment the pass is being drawn for, and the trigger counter that decides
/// whether a countdown's run has started over.
struct Generating<'a> {
    layer_id: LayerId,
    slot_index: u16,
    slot: &'a Slot,
    size: ovs_types::geometry::SizePx,
    /// What the composition is rendered at.
    ///
    /// Only the service pattern has anything to do with it: its caption names
    /// the surface and the mode it is running at, and a caption naming a rate
    /// nobody is running is worse than no caption. Carried here rather than
    /// asked for at the draw, so a composition somebody re-rates gets a new
    /// caption on the next pass.
    rate: ovs_types::geometry::FrameRate,
    trigger: u64,
    now: Instant,
    /// What the surface this composition is shown on has presented, and how
    /// fast it presents.
    ///
    /// **The service pattern's whole clock, and nothing else reads it** (owner,
    /// 2026-10-16: "an nichts gebunden außer die Bildrate des Ausgangs. nicht
    /// an der systemzeit oder sonst was"). Frames the wall really put up, not
    /// passes of the render loop: a machine driving two outputs makes about
    /// two passes a frame, which is how the sweep came to run at double speed.
    shown: Shown,
    /// What a control source has done to the generators, by instance.
    ///
    /// Handed in rather than read from a field because the show state is
    /// borrowed for the whole pass and this is one of its maps.
    overrides: &'a ovs_types::live::GeneratorOverrides,
}

/// What a surface has shown, and how fast it shows it.
///
/// The two halves travel together because neither means anything alone: a
/// count of frames is a time only once it is divided by the rate they were
/// presented at, and dividing by a rate they were *not* presented at is the
/// fault this type exists to make hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// How many frames the surface has presented.
    pub frames: u64,
    /// How fast it presents them.
    pub rate: ovs_types::geometry::FrameRate,
}

/// One layer's generator, kept between passes.
///
/// Kept because two of the published generators draw the time, and a source
/// rebuilt every pass would have nowhere to hold the picture it drew last
///. A still generator costs a struct and nothing else: it is
/// drawn once, uploaded, and never asked again.
#[derive(Debug)]
struct Generated {
    from: GeneratedFrom,
    source: GeneratorSource,
    /// The moment this generator's run is measured from.
    ///
    /// Only a countdown uses it, and only it can move it: a clock is on the wall
    /// clock and a still has no run to be part of.
    anchor: Instant,
    /// The trigger counter the anchor was stamped at.
    trigger: u64,
}

/// One layer's network source, kept between passes.
///
/// Kept for the same reason a generator is: the receiver holds the last picture
/// that arrived, and the layer goes on showing it until a newer one does. A
/// sender with an unchanged picture says nothing for a second at a time —
/// measured at 999 ms between frames from a still test pattern — so a receiver
/// rebuilt every pass, or a layer drawn only when a frame arrived this pass,
/// would show that source as one frame and fifty-nine blacks a second.
struct Receiving {
    /// The source name this was connected for.
    ///
    /// Compared against the slot so that editing the name reconnects. The
    /// receiver cannot be asked what it would be connected to if the show
    /// changed, so the answer is kept beside it.
    source: String,
    receiver: NdiReceiver,
    /// The receiver's arrival count as of the last successful upload.
    ///
    /// The whole per-frame cost of a network layer: one comparison. When it
    /// differs, one upload. A failed upload leaves this behind so the next pass
    /// tries again rather than skipping the frame.
    uploaded: u64,
}

/// A clip asked for and not yet arrived.
///
/// The clock position is the **trigger's**, not the moment the player lands. A
/// clip that took eighty milliseconds to open therefore begins five frames in,
/// which is the truth about when the pad was pressed; starting it at its first
/// frame instead would leave a beat-synced clip permanently that far behind the
/// beat, and every clip on the wall behind by a different amount.
#[derive(Debug, Clone, Copy)]
struct Opening {
    /// What was asked for, which is what a result has to match to be adopted.
    what: OpeningFor,
    /// The beat the trigger was seen on.
    at_beats: f64,
    /// The second the trigger was seen on, for a clip that ignores the tempo.
    at_seconds: f64,
}

/// One layer's playing source.
struct LayerPlayback {
    /// What is playing.
    media_item_id: MediaItemId,
    /// Which slot it came from, so a slot change is noticed even when the two
    /// slots hold the same file.
    slot: u16,
    player: Player,
    /// Where this pass started, and how long it is.
    triggered: Triggered,
    /// How long the clip is, as the decoder that opened it measured it.
    ///
    /// Kept because a report is in the clip's own units — milliseconds and
    /// frames — while [`Triggered`] is in beats, and the two are only
    /// convertible through this.
    length: SourceLength,
    /// The trigger counter this pass was started by.
    trigger: u64,
    /// The frame index currently in the layer's texture.
    uploaded: Option<u32>,
    /// Where the playhead was on the last pass that put a frame on stage.
    ///
    /// Written where the playhead is already worked out, which is once per pass
    /// per layer, and read only when somebody asks for a report — about once a
    /// second. Recomputing it at report time is not possible: the pass it is a
    /// fraction of belongs to the moment the frame was drawn for, and a report
    /// arrives on another thread some milliseconds later (house rule §5).
    ///
    /// `None` while the layer has nothing on stage, so that "no clip" never
    /// leaves a stale bar behind.
    playhead: Option<ovs_types::telemetry::LayerPlayhead>,
    /// The beat this layer's playhead was frozen on, while it is paused.
    ///
    /// `None` when it is playing or stopped. See `transport_beats`, which is
    /// the only thing that reads or writes it.
    held: Option<f64>,
}

/// Which layer is playing what, and which frame of it is on the GPU.
pub struct Playback {
    clock: ShowClock,
    layers: BTreeMap<LayerId, LayerPlayback>,
    /// Where both slow halves of a slot change are done.
    changeover: Changeover,
    /// What each layer is waiting for, while it waits.
    ///
    /// One entry a layer, because a layer has one playhead: a pad pressed three
    /// times while the first open runs has superseded it twice, and the entry
    /// says which of the three the layer is actually waiting for. The other two
    /// players are let go of on arrival rather than put on a wall.
    opening: BTreeMap<LayerId, Opening>,
    /// Every layer this scheduler is responsible for, and whether it has a
    /// frame on the GPU.
    ///
    /// The distinction the scene needs: a clip with a frame samples its own
    /// texture, and a clip without one draws its placeholder. Leaving it out of
    /// the map entirely would make the scene fall back to a shared texture that
    /// a clip never has, which is a white rectangle where the picture should be.
    showing: BTreeMap<LayerId, bool>,
    /// Which kind of source last wrote each layer's texture.
    ///
    /// The one fact none of the three per-source records held; see [`Drawn`].
    /// Written where an upload succeeded rather than where a source was
    /// chosen, so a pass on which nothing changed still costs one comparison.
    drawn: BTreeMap<LayerId, Drawn>,
    /// Each layer's generator, and what it was built from.
    ///
    /// Held so that a generator is drawn when its picture changes and not
    /// otherwise. For five of the seven that is when an operator edits
    /// something: the picture is identical every frame, and filling it sixty
    /// times a second would be work with no output. For the clock and the
    /// countdown it is when the second moves, which is once in sixty passes.
    generated: BTreeMap<LayerId, Generated>,
    /// Layers whose generator could not be built, already complained about.
    complained_generators: BTreeSet<LayerId>,
    /// Each layer's network receiver, and what it was connected for.
    receiving: BTreeMap<LayerId, Receiving>,
    /// Slots holding an NDI source this machine has no runtime for.
    ///
    /// By layer and slot, and said once: the alternative at sixty passes a
    /// second is a log nobody can read.
    complained_ndi: BTreeSet<(LayerId, u16)>,
    /// Slots already warned about for asking more of a decoder than it has.
    ///
    /// By layer and slot rather than by media item: the same clip on two pads
    /// can be set two different ways, and it is the setting that is wrong.
    strained: BTreeSet<(LayerId, u16)>,
    /// Items that could not be opened, and when each last failed.
    ///
    /// Not attempted every frame — a missing file retried sixty times a second
    /// is a syscall storm and a log nobody can read. Not given up on either:
    /// measured on the test machine, a clip moved off the disc and put straight
    /// back never played again, because the item had been written off for the
    /// life of the show. A store on a network mount that blips takes the clip
    /// out of the evening that way.
    failed: BTreeMap<MediaItemId, Instant>,
    /// How many sources have failed since this renderer started.
    ///
    /// Lifetime rather than current, because an operator asking "did anything
    /// go wrong tonight?" is asking about the whole night.
    errors: u64,
    /// Frames held because a decoder was late, for every player that has run.
    ///
    /// Accumulated here rather than read from the players, because a player
    /// that has been retired takes its count with it and the number an operator
    /// watches must not go down.
    late_frames: u64,
    /// What every living player's counts added up to on the last pass.
    ///
    /// Kept so that the rise can be told from the fall that a retiring player
    /// causes. Meaningless on its own, which is why nothing reports it.
    last_late_sum: u64,
    /// What those two counts were when an operator last cleared them.
    ///
    /// The counts themselves keep running — a player has no idea anybody
    /// pressed anything, and the lifetime figure is the answer to "has this
    /// machine ever". What is cleared is the reading: what has happened *since
    /// I looked*, which is the only version of the question worth asking during
    /// a show.
    cleared: ovs_types::telemetry::PlaybackTelemetry,
    /// Stand in for video with a generated clip.
    ///
    /// A development aid, off unless asked for: until a decoder is linked, this
    /// is the only way to drive the whole path — trigger, playhead, player,
    /// upload — on a machine with no codec. It is never a fallback, because a
    /// colour cycle where an operator expected their clip would be a lie on a
    /// wall.
    synthetic: bool,
}

impl Playback {
    /// Starts a scheduler whose clock begins now.
    #[must_use]
    pub fn new(now: Instant, synthetic: bool) -> Self {
        if synthetic {
            tracing::warn!("synthetic video is on: every clip will play a generated test pattern");
        }
        Self {
            clock: ShowClock::new(now),
            layers: BTreeMap::new(),
            // A synthetic source opens no file and builds no decoder, so there
            // is nothing for a pool to take off this thread and a test does not
            // carry four threads it will never ask anything of.
            changeover: if synthetic {
                Changeover::none()
            } else {
                Changeover::spawn()
            },
            opening: BTreeMap::new(),
            generated: BTreeMap::new(),
            complained_generators: BTreeSet::new(),
            receiving: BTreeMap::new(),
            complained_ndi: BTreeSet::new(),
            strained: BTreeSet::new(),
            showing: BTreeMap::new(),
            drawn: BTreeMap::new(),
            failed: BTreeMap::new(),
            errors: 0,
            late_frames: 0,
            last_late_sum: 0,
            cleared: ovs_types::telemetry::PlaybackTelemetry::default(),
            synthetic,
        }
    }

    /// Returns the layers this scheduler drives, and whether each has a frame.
    #[must_use]
    pub const fn showing(&self) -> &BTreeMap<LayerId, bool> {
        &self.showing
    }

    /// Returns the beat position the scheduler is working against.
    #[must_use]
    pub fn beats_at(&self, now: Instant) -> f64 {
        self.clock.beats_at(now)
    }

    /// Advances every layer and puts the frame it needs on the GPU.
    ///
    /// `shown` is what the surface this composition goes to has presented. The
    /// service pattern is drawn from it and nothing else reads it; see
    /// [`Shown`].
    pub fn update(
        &mut self,
        state: &ShowState,
        catalogue: &MediaCatalogue,
        compositor: &mut dyn Compositor,
        now: Instant,
        shown: Shown,
    ) -> PlaybackReport {
        self.clock.follow(&state.master, now);
        let beats = self.clock.beats_at(now);
        // The other clock a pass can be read off. A free-running clip is
        // measured in seconds because it is supposed to ignore the tempo
        // entirely, and reading it off anything the tempo touches is exactly
        // how it came to be playing at four times speed with every control set
        // to one (owner, 2026-09-23).
        let seconds = self.clock.seconds_at(now);
        let bpm = self.clock.bpm();
        // How fast the beat itself is moving: tempo and master speed together,
        // which is what turns a pass measured in beats into a rate a client can
        // animate a progress bar with.
        let beats_per_second = bpm / 60.0 * state.master.speed_scale;

        let mut report = PlaybackReport::default();
        // Every layer that should have a player, whether or not a frame has
        // arrived for it yet. Retiring on "has a frame" instead would stop and
        // restart the player of a layer that had only just been triggered,
        // every pass, so its first frame could never arrive.
        let mut intended: BTreeSet<LayerId> = BTreeSet::new();
        // Every layer whose source is this scheduler's, and whether it has a
        // frame yet. Both halves matter: a quad sampling a texture that is not
        // there falls back to white, which would put a white rectangle on a
        // wall where the layer's placeholder belongs.
        let mut showing: BTreeMap<LayerId, bool> = BTreeMap::new();

        // Before the layers are walked, so a clip that finished opening since the
        // last pass is on the wall on this one rather than on the next.
        self.adopt_opened(state, beats_per_second);

        if let Some(show) = state.show.as_deref() {
            for layer in &show.layers {
                let Some(live) = state.layer(layer.id) else {
                    continue;
                };
                // **Muting takes a layer off the wall, not off the clock**
                // (owner, 2026-09-23): "Mute ist nicht mehr sichtbar, aber soll
                // weiterlaufen. Aktuell starten die Slots jedes mal neu."
                //
                // It used to retire the player, so a muted layer's clip stopped
                // and came back from its first frame — which is the wrong thing
                // for the control an operator reaches for to see what is under
                // a layer for two bars. What is drawn is still gated on both of
                // these, in `scene`; this is only about what keeps running.
                //
                // So a player runs unless the layer is out of the composition
                // *both* ways: switched off in the document and muted at the
                // desk. That is the one case where nobody can bring it back
                // without editing the show, and the decoder and its thread
                // are worth saving (owner decision Q16).
                if !layer.enabled && !live.enabled {
                    continue;
                }

                if live.is_black() {
                    continue;
                }

                let Some(slot) = layer.slot(live.active_slot) else {
                    continue;
                };

                // A generator draws itself, so there is nothing to decode, no
                // playhead to run and no thread to keep alive. A still one is
                // drawn when the slot changes and then left alone; a clock or a
                // countdown is drawn again when its second moves, which is what
                // `generate` decides.
                if matches!(slot.source, SourceRef::Generator { .. }) {
                    // **A generator nobody can see is not drawn.**
                    //
                    // The rule above keeps a layer's *player* running while it
                    // is off in the document or muted at the desk, so a clip
                    // comes back where it would have been rather than at its
                    // first frame. A generator has no playhead to keep: its
                    // picture is a function of its tick, and `tick_at` reads
                    // that from the clock whenever it is next asked. So it can
                    // be left undrawn and picked up again in one pass.
                    //
                    // What it costs otherwise is a full-size picture drawn on
                    // the processor, every frame, for a layer that is switched
                    // off. On the owner's machine on 2026-10-14 that was the
                    // whole of it: with **every layer disabled** the wall still
                    // ran at 15.6 fps and `draw_kind` was still two thirds of
                    // the render thread.
                    //
                    // `intended` still has it, so the generator is not retired
                    // and a countdown's anchor survives being hidden -- which is
                    // the half of owner decision Q16 that does apply here.
                    if !layer.enabled || !live.enabled {
                        intended.insert(layer.id);
                        showing.insert(layer.id, false);
                        continue;
                    }

                    let drawn = self.generate(
                        Generating {
                            layer_id: layer.id,
                            slot_index: live.active_slot,
                            slot,
                            size: show.composition.size,
                            rate: show.composition.render_rate,
                            trigger: live.trigger,
                            now,
                            shown,
                            overrides: &state.generators,
                        },
                        compositor,
                        &mut report,
                    );
                    intended.insert(layer.id);
                    showing.insert(layer.id, drawn);
                    if drawn {
                        report.playing += 1;
                    }
                    continue;
                }

                // A network source has no timeline, so there is no playhead
                // to run and no frame index to schedule: the sender decides
                // what is current and the receiver holds it. What happens here
                // is the same comparison a generator gets.
                if let SourceRef::Ndi { name } = &slot.source {
                    let drawn =
                        self.receive(layer.id, live.active_slot, name, compositor, &mut report);
                    intended.insert(layer.id);
                    showing.insert(layer.id, drawn);
                    if drawn {
                        report.playing += 1;
                    }
                    continue;
                }

                let Some(media_item_id) = slot.media_item_id() else {
                    continue;
                };
                let Some(location) = catalogue.get(&media_item_id) else {
                    continue;
                };
                if location.kind != MediaKind::Video {
                    // Not this scheduler's, so the scene falls back to the
                    // shared texture the residency uploads for it.
                    // A still has no timeline, so it needs no playhead and no
                    // decoder thread. It goes through the residency, which
                    // shares one texture between every layer showing it.
                    continue;
                }
                if self.written_off(media_item_id) {
                    showing.insert(layer.id, false);
                    report.failed += 1;
                    continue;
                }

                // What this trigger is asking for, named the way a result
                // will name it when it comes back.
                let wanted = OpeningFor {
                    layer_id: layer.id,
                    media_item_id,
                    slot: live.active_slot,
                    trigger: live.trigger,
                };
                // An open already in flight for this very trigger *is* this
                // trigger being answered. Without this the request would be made
                // again on every pass until the clip landed — sixty opens for one
                // press, which is the fault this moved off the thread, multiplied.
                let answering = self
                    .opening
                    .get(&layer.id)
                    .is_some_and(|opening| opening.what == wanted);

                // A new source, a new slot, or a new trigger: all three start a
                // pass, and all three are the same decision.
                let restart = !answering
                    && match self.layers.get(&layer.id) {
                        Some(playing) => {
                            playing.media_item_id != media_item_id
                                || playing.slot != live.active_slot
                                || playing.trigger != live.trigger
                        }
                        None => true,
                    };

                if restart {
                    let reuse = self
                        .layers
                        .get(&layer.id)
                        .is_some_and(|playing| playing.media_item_id == media_item_id);

                    if reuse {
                        // The same file, triggered again. Keeping the player
                        // means a retrigger costs nothing: the thread, the ring
                        // and the open file are all still there.
                        // The length before the player, so the pass and the
                        // warning are worked out while nothing is borrowed.
                        let Some(length) = self
                            .layers
                            .get(&layer.id)
                            .map(|playing| playing.player.length())
                        else {
                            continue;
                        };
                        let Some(pass) = pass_for(length, slot.sync, slot.speed) else {
                            self.fail(media_item_id, &mut report);
                            continue;
                        };
                        self.warn_if_unplayable(
                            layer.id,
                            live.active_slot,
                            length,
                            pass,
                            beats_per_second,
                        );
                        let Some(playing) = self.layers.get_mut(&layer.id) else {
                            continue;
                        };
                        playing.slot = live.active_slot;
                        playing.trigger = live.trigger;
                        playing.length = length;
                        playing.triggered = Triggered {
                            started_at: started_in(pass, beats, seconds),
                            pass,
                            frame_count: length.frame_count,
                        };
                    } else {
                        // **A different clip, which is not opened here.** Asked
                        // for instead, and the layer goes on playing whatever it
                        // had until the player lands. The clock position is
                        // stamped now, so the pass is cut to where the pad was
                        // pressed rather than to where the open finished.
                        let opening = Opening {
                            what: wanted,
                            at_beats: beats,
                            at_seconds: seconds,
                        };
                        if self.changeover.open(wanted, location.path.clone()) {
                            self.opening.insert(layer.id, opening);
                        } else if let Some(player) = self.open(media_item_id, location) {
                            // Nobody to hand it to — a synthetic source, which
                            // opens nothing, or a machine that could not spawn
                            // the threads. Opened here, as every trigger was
                            // before the pool existed.
                            self.adopt(player, &opening, state, beats_per_second);
                        } else {
                            self.fail(media_item_id, &mut report);
                            continue;
                        }
                    }
                }

                // **A slot's beats and its rate are live** (owner,
                // 2026-09-23: "Änderungen müssen sofort passieren"). They used
                // to reach a clip only at its next trigger, because the pass
                // was worked out once and kept — so an operator correcting 7
                // beats to 54 on a clip that was running saw nothing happen
                // until they hit the pad again.
                //
                // Read on every pass instead, and the clip re-cut where it
                // stands: same picture, new speed. Compared before anything is
                // touched, so the ordinary case — nobody changed anything — is
                // one equality test and no work.
                if !restart {
                    let current = self
                        .layers
                        .get(&layer.id)
                        .map(|playing| (playing.length, playing.triggered));
                    if let Some((length, triggered)) = current {
                        if let Some(pass) = pass_for(length, slot.sync, slot.speed) {
                            if pass != triggered.pass {
                                // Said again, because what it costs has changed
                                // and the slot may have crossed the line in
                                // either direction.
                                self.warn_if_unplayable(
                                    layer.id,
                                    live.active_slot,
                                    length,
                                    pass,
                                    beats_per_second,
                                );
                                if let Some(playing) = self.layers.get_mut(&layer.id) {
                                    let was = started_in(triggered.pass, beats, seconds);
                                    let now = started_in(pass, beats, seconds);
                                    // A paused clip holds a moment of the old
                                    // clock. It has to be re-stamped in the new
                                    // one, or resuming would jump by whatever
                                    // the two clocks differ by.
                                    let held = playing.held;
                                    playing.triggered.recut(pass, held.unwrap_or(was), now);
                                    if held.is_some() {
                                        playing.held = Some(now);
                                    }
                                }
                            }
                        }
                    }
                }

                let Some(playing) = self.layers.get_mut(&layer.id) else {
                    // A clip still opening for this layer is still wanted, even
                    // with nothing to show for it yet: without this `retire`
                    // would forget the request on the very pass it was made, and
                    // the player would be let go of the moment it arrived.
                    if self.opening.contains_key(&layer.id) {
                        intended.insert(layer.id);
                    }
                    showing.insert(layer.id, false);
                    continue;
                };
                intended.insert(layer.id);

                if playing.player.failed() {
                    playing.playhead = None;
                    if self.failed.insert(media_item_id, Instant::now()).is_none() {
                        self.errors += 1;
                    }
                    showing.insert(layer.id, false);
                    report.failed += 1;
                    continue;
                }

                let playhead = playhead_at(
                    transport_now(
                        playing,
                        live.transport,
                        started_in(playing.triggered.pass, beats, seconds),
                    ),
                    playing.triggered,
                    playback_mode(slot),
                    playback_direction(slot),
                );
                if playhead.ended {
                    // Only `OnceBlack` ends. The layer goes off stage rather
                    // than holding its last frame, which is what that mode is
                    // for.
                    playing.playhead = None;
                    showing.insert(layer.id, false);
                    continue;
                }

                if let Some(frame) = playing.player.frame(playhead.frame) {
                    // The same frame index is only still on the GPU if nothing
                    // else has written this layer's texture since. A clip
                    // paused or holding its last frame, switched away to a
                    // generator and switched back, is the video half of the
                    // same fault -- which the owner had seen before he found
                    // it with a generator: "Das hatte ich aber auch schon mit
                    // Videos das Phaenomen".
                    let ours = self.drawn.get(&layer.id) == Some(&Drawn::Clip);
                    if playing.uploaded != Some(frame.index) || !ours {
                        // Queued rather than copied here: every layer's
                        // picture goes over the bus in the same breath, which
                        // one core cannot do alone.
                        match compositor.queue_texture(
                            TextureKey::Layer(layer.id),
                            std::sync::Arc::clone(&frame.image),
                        ) {
                            Ok(()) => {
                                playing.uploaded = Some(frame.index);
                                self.drawn.insert(layer.id, Drawn::Clip);
                                report.uploaded += 1;
                            }
                            Err(error) => {
                                // The layer keeps whatever frame it had. A wall
                                // holding one frame is better than a wall going
                                // black because one upload failed.
                                tracing::warn!("{error}");
                            }
                        }
                    }
                }

                showing.insert(layer.id, playing.uploaded.is_some());
                if playing.uploaded.is_some() {
                    report.playing += 1;
                    // Recorded here because here is where it has just been
                    // worked out, and nothing later can work it out again: the
                    // pass a playhead is a fraction of belongs to the moment
                    // this frame was drawn for. Nothing is allocated — it is a
                    // few numbers written into a struct this layer has held
                    // since it was triggered — and the list a report is made of
                    // is assembled from these only when one is asked for
                    // (house rule §5).
                    playing.playhead = Some(ovs_types::telemetry::LayerPlayhead {
                        layer_id: layer.id,
                        slot: live.active_slot,
                        media_item_id,
                        frame: playhead.frame,
                        frame_count: playing.length.frame_count,
                        decode_depth: playing.player.decode_depth(),
                        position_ms: position_ms(
                            f64::from(playhead.position),
                            playing.length.duration_ms,
                        ),
                        duration_ms: playing.length.duration_ms,
                        speed_ms_per_second: playhead_speed(
                            playing.triggered,
                            playing.length.duration_ms,
                            started_in(playing.triggered.pass, beats, seconds),
                            beats_per_second,
                            playback_mode(slot),
                            playback_direction(slot),
                        ),
                    });
                } else {
                    // Triggered, but the first frame has not arrived. A bar
                    // drawn for it would be a bar over a layer showing its
                    // placeholder.
                    playing.playhead = None;
                }

                report.late += playing.player.report().late;
            }
        }

        self.retire(&intended, compositor);
        self.showing = showing;
        self.accumulate_late(report.late);
        report
    }

    /// Adds this pass's increase to the count an operator watches.
    ///
    /// `sum` is every living player's lifetime count added together, which is
    /// not a total for the renderer's life: a player that retires takes its
    /// share with it, so the sum falls whenever a show changes or a layer
    /// stops. Taking the highest sum ever seen — which is what this used to do —
    /// looks like accumulation and is not. After a show change the count sits
    /// still while the new players climb back to the old mark, showing an
    /// operator nothing for as long as that takes and then jumping. A soak
    /// caught it doing exactly that: ninety seconds frozen at thirty-five and a
    /// half thousand while three fresh decoders were missing frames the whole
    /// time.
    ///
    /// So what is added is the rise since the last pass, and a fall is read as
    /// players having gone rather than as frames un-missed.
    fn accumulate_late(&mut self, sum: u64) {
        if let Some(rise) = sum.checked_sub(self.last_late_sum) {
            self.late_frames = self.late_frames.saturating_add(rise);
        }
        self.last_late_sum = sum;
    }

    /// Forgets every player, as a show change does.
    ///
    /// The textures go with them: a layer of the show that has just been closed
    /// has no business keeping a frame of it on the GPU.
    pub fn forget_everything(&mut self, compositor: &mut dyn Compositor) {
        for (layer_id, retiring) in std::mem::take(&mut self.layers) {
            // The case that most needs somewhere else to happen: closing a show
            // retires every layer at once, and on the render thread that was a
            // quarter of a second each.
            self.let_go(retiring.player);
            compositor.drop_texture(TextureKey::Layer(layer_id));
        }
        for layer_id in std::mem::take(&mut self.generated).into_keys() {
            compositor.drop_texture(TextureKey::Layer(layer_id));
        }
        // Dropping a receiver closes a connection, which is the one of these
        // three that costs somebody else something: a sender left feeding a
        // receiver nobody reads is bandwidth spent on a show that is closed.
        for layer_id in std::mem::take(&mut self.receiving).into_keys() {
            compositor.drop_texture(TextureKey::Layer(layer_id));
        }
        self.drawn.clear();
        self.complained_generators.clear();
        self.strained.clear();
        self.showing.clear();
        self.failed.clear();
        // Anything still opening was opening for the show that has just closed.
        self.opening.clear();
    }

    /// Puts some of the operator-visible counts back to zero.
    ///
    /// By remembering where they were rather than by zeroing them: the players
    /// keep counting whatever anybody presses, and a count that went backwards
    /// would be a lie about the evening.
    pub fn reset_counters(&mut self, group: ovs_types::telemetry::CounterGroup) {
        use ovs_types::telemetry::CounterGroup;

        if matches!(group, CounterGroup::All | CounterGroup::LateFrames) {
            self.cleared.late_frames = self.late_frames;
        }
        if matches!(group, CounterGroup::All | CounterGroup::DecodeErrors) {
            self.cleared.errors = self.errors;
        }
    }

    /// Returns what to report about playback.
    #[must_use]
    pub fn telemetry(&self) -> ovs_types::telemetry::PlaybackTelemetry {
        ovs_types::telemetry::PlaybackTelemetry {
            // Layers being decoded, not layers showing something. A generator
            // draws itself — there is no thread, no clip and no decode — and
            // counting one as a decoder tells an operator reading the System
            // screen that the machine is carrying work it is not carrying. A
            // show of eight generator layers reported eight decoders.
            playing: self
                .showing
                .iter()
                .filter(|(layer_id, showing)| **showing && self.layers.contains_key(layer_id))
                .count() as u32,
            late_frames: self.late_frames.saturating_sub(self.cleared.late_frames),
            errors: self.errors.saturating_sub(self.cleared.errors),
        }
    }

    /// Fills `into` with where each playing layer's playhead is.
    ///
    /// A buffer rather than a returned list, so a caller reporting once a second
    /// reuses one rather than building one per call. Nothing is worked out here:
    /// every entry was recorded on the pass that drew it, which is the only
    /// moment it could have been (house rule §5).
    ///
    /// A layer showing a generator or showing nothing has no entry. It is not in
    /// this map at all, or its playhead is `None`, and either way the operator
    /// gets no bar rather than a bar at zero.
    pub fn playheads(&self, into: &mut Vec<ovs_types::telemetry::LayerPlayhead>) {
        into.clear();
        into.extend(self.layers.values().filter_map(|playing| playing.playhead));
    }

    /// Draws a layer's generator, if what it should be drawing has changed.
    ///
    /// Returns whether the layer has a frame on the GPU. The cost is a fill of
    /// the composition's size — about eight megabytes at 1080p, well under a
    /// millisecond — and what decides how often it is paid is the generator:
    ///
    /// * A still is drawn when an operator changes something and not otherwise.
    /// * A clock or a countdown is drawn when its second moves, which at 60 Hz
    ///   is one pass in sixty. The other fifty-nine cost a comparison and
    ///   nothing else, and nothing is allocated on any of them: the generator
    ///   writes the new second into a buffer it has held since the slot changed
    ///   (house rule §5).
    fn generate(
        &mut self,
        wanted: Generating<'_>,
        compositor: &mut dyn Compositor,
        report: &mut PlaybackReport,
    ) -> bool {
        let Generating {
            layer_id,
            slot_index,
            slot,
            size,
            rate,
            trigger,
            now,
            shown,
            overrides: wanted_overrides,
        } = wanted;

        let SourceRef::Generator {
            instance_id,
            type_id,
            version,
            parameters,
        } = &slot.source
        else {
            return false;
        };

        // Live over configured, which is the same order every other reader of
        // a plugin's values uses. An instance nobody has touched has no entry
        // and pays nothing for it.
        let overrides = wanted_overrides
            .get(instance_id)
            .cloned()
            .unwrap_or_default();
        let effective = if overrides.is_empty() {
            parameters.clone()
        } else {
            let mut merged = parameters.clone();
            for (key, value) in &overrides {
                merged.insert(key.clone(), value.clone());
            }
            merged
        };

        let from = GeneratedFrom {
            slot: slot_index,
            source: slot.source.clone(),
            size,
            overrides,
        };
        let built = self
            .generated
            .get(&layer_id)
            .is_some_and(|generated| generated.from == from);
        // `built` says this generator was made from the same inputs as last
        // pass. It was being read as "and so its picture is still on the GPU",
        // which is a different fact and the one that was wrong; see [`Drawn`].
        let ours = self.drawn.get(&layer_id) == Some(&Drawn::Generator);

        if !built {
            let Some(source) = GeneratorSource::new(type_id, &effective, size) else {
                // A show validated against this build cannot produce this, so
                // reaching it means a show from somewhere else. The layer
                // draws its placeholder and the reason is said once.
                if self.complained_generators.insert(layer_id) {
                    self.errors += 1;
                    tracing::warn!(
                        type_id,
                        version,
                        "this build does not publish that generator; the layer will draw its placeholder"
                    );
                }
                report.failed += 1;
                self.generated.remove(&layer_id);
                return false;
            };

            self.generated.insert(
                layer_id,
                Generated {
                    from,
                    source,
                    anchor: now,
                    trigger,
                },
            );
        }

        let Some(generated) = self.generated.get_mut(&layer_id) else {
            return false;
        };

        // An operator hitting the pad again. It starts a countdown over if that
        // is what the countdown was set up to do, and moves nothing otherwise:
        // a clock is on the wall clock and a still has no run to restart.
        if generated.trigger != trigger {
            if generated.source.restarts_on_trigger() {
                generated.anchor = now;
            }
            generated.trigger = trigger;
        }

        // Somebody else has written this layer's texture. The buffer a moving
        // generator keeps is still right, so only its belief about the GPU is
        // dropped -- and dropping it here is what lets the `None` arm below go
        // on returning `built`, because a forgotten buffer always redraws.
        if !ours {
            generated.source.forget_drawn();
        }

        if !generated.source.changes_over_time() {
            if built && ours {
                // Drawn already, and it draws the same thing every time.
                return true;
            }
            let Ok(frame) = generated.source.frame(0) else {
                report.failed += 1;
                self.generated.remove(&layer_id);
                return false;
            };
            return match compositor.upload_texture(TextureKey::Layer(layer_id), &frame.image) {
                Ok(()) => {
                    self.complained_generators.remove(&layer_id);
                    self.drawn.insert(layer_id, Drawn::Generator);
                    report.uploaded += 1;
                    true
                }
                Err(error) => {
                    // Forgotten rather than kept, so the next pass tries again.
                    // A layer holding nothing must not also believe it is drawn.
                    tracing::warn!("{error}");
                    self.generated.remove(&layer_id);
                    false
                }
            };
        }

        let moment = Moment {
            wall: SystemTime::now(),
            since_anchor: now.saturating_duration_since(generated.anchor),
            frames: shown.frames,
        };
        let tick = generated.source.tick_at(moment).unwrap_or(0);

        // **The card draws it, where it can.** Nothing is drawn on the
        // processor and nothing crosses the bus: measured on PC05 on
        // 2026-10-15, a 1920x1080 noise field cost the render thread 25,532
        // microseconds a pass, against six for a 4K clip decoded on the card.
        //
        // Drawn every pass rather than only when the tick moved, as the
        // processor's path does. The reason that path keeps a tick is that
        // drawing cost twenty-five milliseconds; here it is a triangle, and
        // redrawing something that has not changed is cheaper than owning the
        // question of whether what is on the card is still what was asked for.
        // The service pattern reads a picture of its own unmoving parts, so it
        // goes through its own door: the picture is uploaded on the pass that
        // rebuilt it and on no other, and then the card drifts, sweeps and
        // captions it every pass for nothing.
        if let Some((fresh, moment)) = generated.source.pattern_on_card(tick, rate, shown.rate) {
            let source = TextureKey::PatternSource(layer_id);
            let ready = match fresh {
                Some(picture) => match compositor.upload_texture(source, &picture) {
                    Ok(()) => true,
                    Err(error) => {
                        // Without its bars the card would draw the sweep over
                        // nothing, so this falls through to the processor.
                        tracing::warn!("{error}");
                        false
                    }
                },
                None => true,
            };
            if ready {
                let what = GeneratedDraw::Pattern {
                    shift: [moment.shift_x, moment.shift_y],
                    sweep_start: moment.sweep_start,
                    sweep_trail: moment.sweep_trail,
                    label_side: moment.label_side,
                    source,
                };
                match compositor.draw_generated(
                    TextureKey::Layer(layer_id),
                    generated.source.size(),
                    what,
                ) {
                    Ok(true) => {
                        self.complained_generators.remove(&layer_id);
                        self.drawn.insert(layer_id, Drawn::Generator);
                        return true;
                    }
                    Ok(false) => {}
                    Err(error) => tracing::warn!("{error}"),
                }
            }
        }

        if let Some(what) = generated.source.on_card(tick) {
            match compositor.draw_generated(
                TextureKey::Layer(layer_id),
                generated.source.size(),
                what,
            ) {
                Ok(true) => {
                    self.complained_generators.remove(&layer_id);
                    self.drawn.insert(layer_id, Drawn::Generator);
                    return true;
                }
                // This backend does not draw generators. The processor does,
                // below, exactly as it always did.
                Ok(false) => {}
                Err(error) => {
                    // Not fatal: fall through and draw it the slow way. A wall
                    // that is slower is a wall; one that is black is not.
                    tracing::warn!("{error}");
                }
            }
        }

        let uploaded = match generated.source.redraw_at(tick, rate, shown.rate) {
            Some(image) => compositor.upload_texture(TextureKey::Layer(layer_id), image),
            // The second has not moved, so what is on the GPU is still right.
            None => return built,
        };

        match uploaded {
            Ok(()) => {
                self.complained_generators.remove(&layer_id);
                self.drawn.insert(layer_id, Drawn::Generator);
                report.uploaded += 1;
                true
            }
            Err(error) => {
                // Forgotten rather than kept, so the next pass builds it again
                // and draws. A generator that believed it had drawn a second it
                // had not would hold that belief until the second moved, and a
                // layer the scene thinks is showing something while holding
                // nothing puts a white rectangle where its placeholder belongs.
                tracing::warn!("{error}");
                self.generated.remove(&layer_id);
                false
            }
        }
    }

    /// Says so when a pass asks for more frames than a decoder is likely to give.
    ///
    /// **Once per slot**, when it is triggered and again whenever its pass is
    /// re-cut under it, never once a frame. A clip set this way misses on
    /// nearly every pass — the owner's was holding a stale frame about a
    /// hundred and seventy times a second — so a line per miss would be a log
    /// nobody can read and a disc nobody asked to fill.
    ///
    /// A warning and never a refusal. The renderer plays what it is given,
    /// and what the machine can decode is a fact about the machine, not
    /// a rule about the show. What it can do is say the number, because nothing
    /// else in the system was saying it: the late-frame counter climbed and
    /// gave no reason, and an operator looking at a wall that had stopped had
    /// no way to find out that seven beats on a seventy-five-second clip is
    /// twenty-one times speed.
    fn warn_if_unplayable(
        &mut self,
        layer_id: LayerId,
        slot: u16,
        length: SourceLength,
        pass: Pass,
        beats_per_second: f64,
    ) {
        let fps = demanded_fps(length, pass, beats_per_second);
        if fps <= crate::media::FPS_WORTH_WARNING_ABOUT {
            // Back within reach — the tempo came down, or the beats went up.
            // Forgotten, so the next time it goes over is said again.
            self.strained.remove(&(layer_id, slot));
            return;
        }
        if !self.strained.insert((layer_id, slot)) {
            return;
        }

        let own = length.frame_count as f64 * 1_000.0 / (length.duration_ms.max(1) as f64);
        tracing::warn!(
            %layer_id,
            slot,
            demanded_fps = fps.round(),
            clip_fps = own.round(),
            times_speed = (fps / own.max(1.0) * 10.0).round() / 10.0,
            "this pass asks for more frames a second than a decoder is likely \
             to give; the picture will hold frames rather than play faster"
        );
    }

    /// Whether an item is not worth trying again just yet.
    ///
    /// The cooling-off is what keeps a missing file from being opened on every
    /// pass. It is short because the thing on the other side of it is an
    /// operator pressing a pad again, and because a store that has come back
    /// should start playing without anybody restarting anything.
    fn written_off(&self, media_item_id: MediaItemId) -> bool {
        /// Long enough that a frame loop cannot hammer a missing file, short
        /// enough that a second press of the pad is a fresh attempt.
        const COOLING_OFF: std::time::Duration = std::time::Duration::from_secs(2);

        self.failed
            .get(&media_item_id)
            .is_some_and(|at| at.elapsed() < COOLING_OFF)
    }

    /// Records a source as unusable, and counts it against this pass.
    ///
    /// For a failure found while the layers are being walked: the loop is already
    /// past its check for this layer, so nothing else will count it.
    fn fail(&mut self, media_item_id: MediaItemId, report: &mut PlaybackReport) {
        self.write_off(media_item_id);
        report.failed += 1;
    }

    /// Records a source as unusable without counting it against this pass.
    ///
    /// For a failure that arrives *before* the layers are walked, which is where
    /// an open on a worker fails. The loop will find the item written off and
    /// count it once, as it counts every written-off item; counting it here as
    /// well reported two failed layers where there is one.
    ///
    /// Said once, however often the item is tried again — and it is tried every
    /// couple of seconds, because a clip moved off the disc and put straight back
    /// has to play again without anybody restarting anything.
    fn write_off(&mut self, media_item_id: MediaItemId) {
        if self.failed.insert(media_item_id, Instant::now()).is_none() {
            self.errors += 1;
            tracing::warn!(%media_item_id, "nothing here can play this item");
        }
    }

    /// Forgets that an item ever failed, and says so.
    ///
    /// Called when one opens after having failed. Worth a line: an operator who
    /// saw the first warning is entitled to know the clip is back, and it is the
    /// only evidence that a store returned.
    fn recovered(&mut self, media_item_id: MediaItemId) {
        if self.failed.remove(&media_item_id).is_some() {
            tracing::info!(%media_item_id, "this item can be played again");
        }
    }

    /// Lets go of a player somewhere other than here.
    ///
    /// Dropping one joins its decode thread, which does not finish until the
    /// decoder it owns is torn down: a quarter of a second for a hardware one,
    /// measured, which is sixteen frames (`crate::media::changeover`). Dropped
    /// here only when there is nobody to hand it to.
    fn let_go(&self, player: Player) {
        if let Some(player) = self.changeover.retire(player) {
            drop(player);
        }
    }

    /// Takes in the clips that have finished opening.
    ///
    /// Everything that can have happened while one was opening happens here: the
    /// pad pressed again, the slot changed, the layer taken out of the show, the
    /// file turning out to be unreadable. A player nobody is waiting for any more
    /// is let go of rather than put on a wall the operator has moved on from.
    fn adopt_opened(&mut self, state: &ShowState, beats_per_second: f64) {
        // A local rather than a field: `Vec::new` allocates nothing until
        // something is pushed, and on the overwhelming majority of passes
        // nothing is.
        let mut arrived = Vec::new();
        self.changeover.collect(&mut arrived);

        for Opened { what, outcome } in arrived {
            let waiting = self
                .opening
                .get(&what.layer_id)
                .copied()
                .filter(|opening| opening.what == what);
            let Some(opening) = waiting else {
                // Superseded, or the layer is gone. Nothing is said about it: an
                // operator pressing a pad twice has not caused a fault.
                if let Ok(player) = outcome {
                    self.let_go(player);
                }
                continue;
            };
            self.opening.remove(&what.layer_id);

            match outcome {
                Ok(player) => self.adopt(player, &opening, state, beats_per_second),
                Err(error) => {
                    // The same two things the inline path says, in the same two
                    // ways. Once per outage for a file, because an item is
                    // retried every couple of seconds and a line each time would
                    // be eighteen hundred an hour saying the same thing; every
                    // time for a machine that cannot spawn a thread, because that
                    // is not an outage anything recovers from on its own.
                    match &error {
                        NotOpened::Source(_) => {
                            if !self.failed.contains_key(&what.media_item_id) {
                                tracing::warn!(media_item_id = %what.media_item_id, "{error}");
                            }
                        }
                        NotOpened::NoThread(_) => {
                            tracing::error!(media_item_id = %what.media_item_id, "{error}");
                        }
                    }
                    // Written off rather than counted: this runs before the
                    // layers are walked, and the loop counts it.
                    self.write_off(what.media_item_id);
                }
            }
        }
    }

    /// Puts an opened clip on its layer, and lets go of what was there.
    ///
    /// The one place a player becomes a layer's, whether it was opened on a
    /// worker or on this thread, so the pass, the warning and the retirement of
    /// the outgoing clip cannot come to differ between the two.
    fn adopt(
        &mut self,
        player: Player,
        opening: &Opening,
        state: &ShowState,
        beats_per_second: f64,
    ) {
        let what = opening.what;

        // The slot as the show has it *now*. A show edited while the clip opened
        // is the ordinary case in a rehearsal, and a pass has to be cut to what
        // the slot says today rather than to what it said when the pad was
        // pressed.
        let slot = state
            .show
            .as_deref()
            .and_then(|show| show.layers.iter().find(|layer| layer.id == what.layer_id))
            .and_then(|layer| layer.slot(what.slot));
        let Some(slot) = slot else {
            self.let_go(player);
            return;
        };

        // It opened, so whatever was wrong with it is not wrong any more.
        self.recovered(what.media_item_id);
        // The file's own length, now that something has read it. The catalogue's
        // numbers were written at ingest and are a guess by comparison; a pass
        // built on them would run a clip to the wrong beat.
        let length = player.length();
        let Some(pass) = pass_for(length, slot.sync, slot.speed) else {
            // Nothing can put a playhead on a clip with no length. Written off
            // like a failed decode, and left for the loop to count — on this pass
            // when the clip was opened on a worker, and on the next when it was
            // opened inline, because the loop is already past this layer by then.
            self.write_off(what.media_item_id);
            self.let_go(player);
            return;
        };
        self.warn_if_unplayable(what.layer_id, what.slot, length, pass, beats_per_second);

        // The outgoing clip, which has been on the wall the whole time this one
        // was opening. Let go of elsewhere: it is the expensive half.
        if let Some(outgoing) = self.layers.remove(&what.layer_id) {
            self.let_go(outgoing.player);
        }
        self.layers.insert(
            what.layer_id,
            LayerPlayback {
                media_item_id: what.media_item_id,
                slot: what.slot,
                player,
                triggered: Triggered {
                    started_at: started_in(pass, opening.at_beats, opening.at_seconds),
                    pass,
                    frame_count: length.frame_count,
                },
                length,
                trigger: what.trigger,
                uploaded: None,
                playhead: None,
                held: None,
            },
        );
    }

    /// Opens a source for an item, if this build can play it, **on this thread**.
    ///
    /// The fallback, and the synthetic path. Opening a real clip costs between one
    /// and five frames (`crate::media::changeover`), so a trigger goes to the pool
    /// and reaches this only when there is no pool to go to — a machine that could
    /// not spawn the threads — or when the source opens no file at all, which is
    /// what a synthetic one is.
    fn open(
        &self,
        media_item_id: MediaItemId,
        location: &ovs_ipc::MediaLocation,
    ) -> Option<Player> {
        if self.synthetic {
            // A test's own source: a known number of known frames, with no file
            // behind it. Only tests turn this on.
            let length = length_of(location, true);
            let size = location
                .dimensions
                .unwrap_or(ovs_types::geometry::SizePx::new(640, 360));
            return Player::start(
                media_item_id,
                Box::new(SyntheticSource::new(
                    size,
                    length.duration_ms,
                    length.frame_count,
                )),
            )
            .ok();
        }

        #[cfg(unix)]
        {
            match crate::media::VideoSource::open(&location.path) {
                Ok(source) => {
                    let length = source.length();
                    tracing::info!(
                        %media_item_id,
                        path = %location.path.display(),
                        width = source.size().width,
                        height = source.size().height,
                        frames = length.frame_count,
                        duration_ms = length.duration_ms,
                        "clip opened"
                    );
                    Player::start(media_item_id, Box::new(source))
                        .inspect_err(|error| {
                            tracing::error!(%media_item_id, "cannot start a player: {error}");
                        })
                        .ok()
                }
                Err(error) => {
                    // The layer draws its placeholder and the log says which
                    // file and why — the same answer a missing file gets, and
                    // better than a stand-in nobody asked for.
                    //
                    // Once per outage, not once per retry. An item is retried
                    // every couple of seconds so that a store coming back plays
                    // without anybody restarting anything, and a line each time
                    // would be eighteen hundred an hour saying the same thing.
                    // It is said again when the item has recovered and failed
                    // afresh, because that is news.
                    if !self.failed.contains_key(&media_item_id) {
                        tracing::warn!(%media_item_id, "{error}");
                    }
                    None
                }
            }
        }

        #[cfg(not(unix))]
        {
            // No decoder is compiled into this build: the FFmpeg libraries are
            // a Unix dependency and this is a development machine.
            tracing::warn!(
                %media_item_id,
                path = %location.path.display(),
                "this build has no video decoder; the layer will draw its placeholder"
            );
            None
        }
    }

    /// Keeps one layer's network receiver, and uploads when a frame has arrived.
    ///
    /// Returns whether the layer has a picture on the GPU. That is not the same
    /// as "a frame arrived this pass": a sender showing an unchanged picture
    /// sends nothing for a second at a time, and the layer is supposed to go on
    /// showing what it last received.
    fn receive(
        &mut self,
        layer_id: LayerId,
        slot_index: u16,
        source: &str,
        compositor: &mut dyn Compositor,
        report: &mut PlaybackReport,
    ) -> bool {
        let ndi = match Ndi::load() {
            Ok(ndi) => ndi,
            Err(error) => {
                // The runtime is installed by the operator rather than carried
                // by the package — see `crate::ndi` for why — so its absence is
                // an ordinary state of a machine rather than a fault in the
                // show. Said once per layer and slot all the same: a dark layer
                // with no explanation anywhere is the fault this codebase is
                // written against.
                if self.complained_ndi.insert((layer_id, slot_index)) {
                    self.errors += 1;
                    tracing::warn!(
                        layer = %layer_id,
                        slot = slot_index,
                        source,
                        "{error}; the layer draws its placeholder"
                    );
                }
                report.failed += 1;
                self.receiving.remove(&layer_id);
                return false;
            }
        };

        // Whether the layer's texture still holds this receiver's own frame. A
        // live source with nothing new to send is where this matters: its own
        // record is of arrivals, and no arrival means no upload, so switching
        // to a generator and back showed the generator until the sender
        // happened to produce a frame.
        let ours = self.drawn.get(&layer_id) == Some(&Drawn::Ndi);

        // A different name is a different connection. A new *trigger* is not:
        // there is nothing to start over in a live source, and reconnecting on
        // it would put a gap on the wall every time an operator hit the pad.
        let connected = self
            .receiving
            .get(&layer_id)
            .is_some_and(|holding| holding.source == source);
        if !connected {
            self.receiving.insert(
                layer_id,
                Receiving {
                    source: source.to_owned(),
                    receiver: NdiReceiver::connect(ndi, source),
                    uploaded: 0,
                },
            );
        }

        let Some(holding) = self.receiving.get_mut(&layer_id) else {
            return false;
        };

        // The whole per-frame cost of a network layer when nothing has changed.
        let arrived = holding.receiver.arrived();
        if arrived == holding.uploaded && ours {
            // `uploaded` starts at zero and arrivals count from one, so this is
            // also "nothing has arrived yet" — a layer waiting for a sender
            // somebody has not switched on, whose placeholder is the right
            // thing to be showing.
            return holding.uploaded > 0;
        }
        let Some(frame) = holding.receiver.frame() else {
            // Nothing to put there. With somebody else's picture on the
            // texture, saying this layer is showing would claim it as this
            // source's; the placeholder is the honest thing to draw.
            return holding.uploaded > 0 && ours;
        };

        match compositor.upload_texture(TextureKey::Layer(layer_id), &frame) {
            Ok(()) => {
                holding.uploaded = arrived;
                self.drawn.insert(layer_id, Drawn::Ndi);
                self.complained_ndi.remove(&(layer_id, slot_index));
                report.uploaded += 1;
                true
            }
            Err(error) => {
                // `uploaded` is left where it was, so the next pass tries again
                // — with whatever the newest frame is by then, which for a live
                // source is the only one worth having.
                tracing::warn!("{error}");
                holding.uploaded > 0 && ours
            }
        }
    }

    /// Stops the players for layers that are no longer meant to be playing.
    fn retire(&mut self, intended: &BTreeSet<LayerId>, compositor: &mut dyn Compositor) {
        let gone: Vec<LayerId> = self
            .layers
            .keys()
            .chain(self.generated.keys())
            .chain(self.receiving.keys())
            .copied()
            .filter(|layer_id| !intended.contains(layer_id))
            .collect();

        for layer_id in gone {
            // Dropping the player stops its thread and closes its file, and the
            // texture goes with it: what it holds is a frame of something this
            // layer is no longer showing. Dropping a receiver does the same to
            // a network connection, which matters more than closing a file: a
            // sender left feeding a receiver nobody reads is bandwidth spent on
            // nothing, all evening.
            if let Some(retiring) = self.layers.remove(&layer_id) {
                self.let_go(retiring.player);
            }
            self.generated.remove(&layer_id);
            self.receiving.remove(&layer_id);
            self.drawn.remove(&layer_id);
            compositor.drop_texture(TextureKey::Layer(layer_id));
        }

        // A clip opening for a layer the show no longer has is a player nobody
        // will adopt. Forgotten here, so it is let go of when it arrives rather
        // than put on a wall that has moved on.
        self.opening
            .retain(|layer_id, _| intended.contains(layer_id));
    }
}

/// Which beat a layer's playhead should be read at, given its transport.
///
/// The playhead is a function of the beat, so pausing is not a flag the
/// timeline checks — it is asking the timeline about a different moment. A
/// paused layer is asked about the beat it paused on, for as long as it stays
/// paused, so it holds the frame it was showing.
///
/// # Resuming carries on rather than catching up
///
/// The owner's requirement is that resume continues "from that exact position"
/// (owner decision Q16), so the pass is moved forward by however long the pause
/// lasted. The alternative — leaving the pass where it was — would make resume
/// jump to wherever the music had got to, which is a scrub rather than a
/// resume.
///
/// What that costs is the beat lock: a clip aligned to the bar is no longer
/// aligned to it after a pause, by exactly the length of the pause. That is
/// inherent in having asked the clip to wait.
///
/// # Stopped is re-anchored rather than remembered
///
/// A stopped layer has its pass moved to now on every pass, so the playhead
/// reads the beginning and going back to `Playing` starts the clip rather than
/// dropping into the middle of it.
/// The clock a pass is read off: seconds for a free-running clip, beats for a
/// synced one.
///
/// Named, because it is the whole of the difference between the two and it has
/// to give the same answer in three places — where a pass is started, where the
/// playhead is read, and where a pause is unwound.
const fn started_in(pass: Pass, beats: f64, seconds: f64) -> f64 {
    match pass {
        Pass::Seconds(_) => seconds,
        Pass::Beats(_) => beats,
    }
}

fn transport_now(playing: &mut LayerPlayback, transport: Transport, now: f64) -> f64 {
    match transport {
        Transport::Playing => {
            if let Some(held) = playing.held.take() {
                playing.triggered.started_at += now - held;
            }
            now
        }
        Transport::Paused => *playing.held.get_or_insert(now),
        Transport::Stopped => {
            playing.held = None;
            playing.triggered.started_at = now;
            now
        }
    }
}

/// How long a clip is, and how many frames that makes.
///
/// The catalogue's numbers are advisory (they come from the database, which can
/// be out of date), so a missing rate falls back to something a timeline can be
/// built on rather than refusing to play. A decoder that opens the file and
/// disagrees wins.
fn length_of(location: &ovs_ipc::MediaLocation, synthetic: bool) -> SourceLength {
    const FALLBACK_DURATION_MS: u64 = 4_000;
    const FALLBACK_FPS: f64 = 25.0;

    let duration_ms = location.duration_ms.filter(|ms| *ms > 0).unwrap_or({
        if synthetic {
            FALLBACK_DURATION_MS
        } else {
            0
        }
    });

    let fps = location
        .frame_rate
        .map(|rate| f64::from(rate.numerator) / f64::from(rate.denominator.max(1)))
        .filter(|fps| *fps > 0.0)
        .unwrap_or(FALLBACK_FPS);

    let frames = (duration_ms as f64 * fps / 1_000.0).round();
    SourceLength {
        duration_ms,
        frame_count: (frames as u32).max(1),
    }
}

/// Turns a position within a pass into a place in the clip, in milliseconds.
///
/// The number an operator reads. Clamped to the clip's own length, so a position
/// of exactly `1.0` is the end of the clip rather than one rounding past it.
fn position_ms(position: f64, duration_ms: u64) -> u64 {
    if !position.is_finite() || position <= 0.0 || duration_ms == 0 {
        return 0;
    }
    // `as` saturates at the integer's bounds for a finite float.
    ((position * duration_ms as f64).round() as u64).min(duration_ms)
}

/// How fast a playhead is travelling through its clip, and which way.
///
/// In clip milliseconds per second of real time, so a thousand is real time and
/// five hundred is half speed. What a client animates a progress bar with
/// between samples.
///
/// It is deliberately not the layer's speed ratio on its own. A pass is measured
/// in beats and kept from the trigger, so raising the tempo afterwards makes the
/// clip play faster without anything about the layer having changed — and a
/// client that multiplied the ratio by the master speed would miss that
/// entirely. Dividing the beat rate by the pass length assumes none of it.
///
/// Signed, because a timeline can run backwards, and zero wherever the playhead
/// has stopped: a clip that has played once and frozen is holding its last
/// frame, and a bar that carried on over it would be an animation of nothing.
fn playhead_speed(
    triggered: Triggered,
    duration_ms: u64,
    now: f64,
    beats_per_second: f64,
    mode: PlaybackMode,
    direction: PlaybackDirection,
) -> f64 {
    let pass = triggered.pass.length();
    if !pass.is_finite() || pass <= 0.0 || duration_ms == 0 {
        return 0.0;
    }
    // A pass measured in seconds is already a rate: one second of the clip a
    // second, which is what free run means. A pass measured in beats has to be
    // turned into one, and that is where the tempo comes in.
    let per_unit = match triggered.pass {
        Pass::Seconds(_) => 1.0,
        Pass::Beats(_) => beats_per_second,
    };
    if !per_unit.is_finite() {
        return 0.0;
    }

    let elapsed = now - triggered.started_at;
    if !elapsed.is_finite() || elapsed <= 0.0 {
        // A pass that starts ahead of the moment being asked about holds its
        // first frame, and a bar for it must sit still rather than run.
        return 0.0;
    }

    let completed = (elapsed / pass).floor();
    if matches!(mode, PlaybackMode::OnceFreeze | PlaybackMode::OnceBlack) && completed >= 1.0 {
        return 0.0;
    }

    let speed = duration_ms as f64 * per_unit / pass;
    // A bounce runs its odd passes backwards and a reversed slot mirrors the
    // whole timeline, so the two together run forwards again.
    let bouncing_back = matches!(mode, PlaybackMode::Bounce) && completed % 2.0 >= 1.0;
    if bouncing_back != matches!(direction, PlaybackDirection::Reverse) {
        -speed
    } else {
        speed
    }
}

/// The playback mode of a slot.
fn playback_mode(slot: &Slot) -> PlaybackMode {
    slot.playback_mode
}

/// The playback direction of a slot.
fn playback_direction(slot: &Slot) -> PlaybackDirection {
    slot.direction
}

#[cfg(test)]
mod tests {

    /// A surface for a pass that does not care what has been shown on it.
    ///
    /// Only the service pattern reads this, and only to work out how far
    /// into its own run it is.
    fn a_wall() -> Shown {
        Shown {
            frames: 0,
            rate: ovs_types::geometry::FrameRate::FPS_60,
        }
    }
    use std::path::PathBuf;
    use std::time::Duration;

    use ovs_ipc::MediaLocation;
    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::geometry::{FrameRate, SizePx};
    use ovs_types::live::{LayerLive, MasterLive};

    use super::*;
    use crate::backend::HeadlessCompositor;

    fn location(media_item_id: MediaItemId, kind: MediaKind) -> MediaLocation {
        MediaLocation {
            media_item_id,
            kind,
            path: PathBuf::from("clip.mov"),
            dimensions: Some(SizePx::new(16, 9)),
            duration_ms: Some(4_000),
            frame_rate: Some(FrameRate {
                numerator: 25,
                denominator: 1,
            }),
        }
    }

    /// A show with one layer on slot 1, playing `media_item_id`.
    fn show(media_item_id: MediaItemId) -> (ShowState, LayerId) {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, media_item_id));
        let layer_id = layer.id;
        show.layers.push(layer);

        let mut live = LayerLive::black(layer_id);
        live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![live],
            ..ShowState::default()
        };
        (state, layer_id)
    }

    fn catalogue(media_item_id: MediaItemId, kind: MediaKind) -> MediaCatalogue {
        let mut catalogue = MediaCatalogue::new();
        catalogue.insert(media_item_id, location(media_item_id, kind));
        catalogue
    }

    #[test]
    fn only_a_layer_with_a_decoder_behind_it_is_counted_as_one() {
        // The System screen shows this number as "active decoders", and an
        // operator reads it to know what the machine is carrying. A generator
        // costs a draw and nothing else — no thread, no clip, no decode — so a
        // show built out of them reported a load that was not there.
        let mut playback = Playback::new(Instant::now(), true);

        playback.showing.insert(LayerId::new(), true);
        playback.showing.insert(LayerId::new(), true);
        assert_eq!(
            playback.telemetry().playing,
            0,
            "two layers are showing and neither has a player behind it"
        );
    }

    #[test]
    fn the_late_count_keeps_counting_after_the_players_that_earned_it_have_gone() {
        // What arrives each pass is every living player's lifetime count added
        // together, and that falls when a show change retires them. Taking
        // the highest sum ever seen looks like accumulation and is not: a soak
        // caught the number frozen for ninety seconds after a show change
        // while three fresh decoders missed frames the whole time.
        let mut playback = Playback::new(Instant::now(), true);

        playback.accumulate_late(0);
        playback.accumulate_late(40);
        playback.accumulate_late(100);
        assert_eq!(playback.telemetry().late_frames, 100);

        // The show changes: three players go, three arrive at zero.
        playback.accumulate_late(0);
        assert_eq!(
            playback.telemetry().late_frames,
            100,
            "what was missed does not become un-missed"
        );

        // And the new players start missing frames, which has to show at once
        // rather than only once they pass the old mark.
        playback.accumulate_late(7);
        assert_eq!(playback.telemetry().late_frames, 107);
        playback.accumulate_late(9);
        assert_eq!(playback.telemetry().late_frames, 109);
    }

    #[test]
    fn clearing_the_late_count_does_not_stop_it_counting() {
        // Cleared by remembering where it was, so a later pass still adds to it
        // and what an operator sees starts again from zero.
        let mut playback = Playback::new(Instant::now(), true);
        playback.accumulate_late(50);
        playback.reset_counters(ovs_types::telemetry::CounterGroup::LateFrames);
        assert_eq!(playback.telemetry().late_frames, 0);

        playback.accumulate_late(60);
        assert_eq!(playback.telemetry().late_frames, 10);
    }

    #[test]
    fn a_source_that_failed_is_tried_again_once_it_has_cooled_off() {
        // Measured on the test machine: a clip moved off the disc and put
        // straight back never played again, because the item had been written
        // off for the life of the show. A store on a network mount that blips
        // takes the clip out of the evening that way.
        let mut playback = Playback::new(Instant::now(), true);
        let item = MediaItemId::new();

        playback.failed.insert(item, Instant::now());
        assert!(
            playback.written_off(item),
            "not retried on the very next pass, or a missing file is opened sixty times a second"
        );

        let long_ago = Instant::now()
            .checked_sub(std::time::Duration::from_secs(5))
            .expect("this process has been running for five seconds' worth of clock");
        playback.failed.insert(item, long_ago);
        assert!(
            !playback.written_off(item),
            "and tried again by the time somebody presses the pad a second time"
        );
    }

    #[test]
    fn a_source_that_failed_twice_is_still_one_thing_that_went_wrong() {
        // The count an operator reads at the end of the night is how many
        // sources failed, not how many times each was retried.
        let mut playback = Playback::new(Instant::now(), true);
        let item = MediaItemId::new();
        let mut report = PlaybackReport::default();

        playback.fail(item, &mut report);
        playback.fail(item, &mut report);

        assert_eq!(playback.errors, 1, "one item, one error");
    }

    #[test]
    fn a_source_that_comes_back_is_forgotten_rather_than_held_against_it() {
        let mut playback = Playback::new(Instant::now(), true);
        let item = MediaItemId::new();
        let mut report = PlaybackReport::default();

        playback.fail(item, &mut report);
        assert!(playback.written_off(item));

        playback.recovered(item);
        assert!(
            !playback.written_off(item),
            "an item that opened is not a failed item"
        );
    }

    #[test]
    fn a_still_is_left_to_the_residency() {
        // A still has no timeline, so a playhead and a decoder thread would be
        // machinery doing nothing, and a texture per layer would upload one
        // backdrop six times.
        let item = MediaItemId::new();
        let (state, _) = show(item);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        let report = playback.update(
            &state,
            &catalogue(item, MediaKind::Image),
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(report.playing, 0);
        assert!(playback.showing().values().all(|showing| !*showing));
    }

    #[test]
    fn a_build_with_no_decoder_leaves_the_layer_on_its_placeholder() {
        // A source that cannot be played is a fault for that source, said once,
        // not a stand-in nobody asked for.
        //
        // Several passes, because opening is not done on this thread: the answer
        // — a player, or the reason there is none — arrives on a later pass
        // (`crate::media::changeover`). Asserting on the first pass would be
        // asserting that the scheduler waits for it, which is the one thing it
        // must not do. So this is also the end-to-end test of that path: asked
        // for on a worker, failed there, adopted here, counted once.
        let item = MediaItemId::new();
        let (state, _) = show(item);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = catalogue(item, MediaKind::Video);

        let mut report = PlaybackReport::default();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while report.failed == 0 && Instant::now() < deadline {
            report = playback.update(
                &state,
                &catalogue,
                &mut compositor,
                Instant::now(),
                a_wall(),
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert_eq!(report.failed, 1, "the unplayable source was never reported");
        assert_eq!(report.playing, 0);
        assert_eq!(compositor.texture_count(), 0, "nothing was uploaded");
    }

    /// The committed clip: ten frames of red, ten of green, ten of blue, at 25
    /// fps, so where the playhead is can be read off a single pixel.
    #[cfg(unix)]
    const CLIP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/media/colours.mov");

    /// The same thirty frames as eight-bit H.264, for the tests that need a
    /// clip of each depth rather than a clip of each container.
    #[cfg(unix)]
    const LONG_GOP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/media/colours-h264.mp4");

    /// Runs the scheduler at one instant until a frame reaches the GPU.
    ///
    /// The decode is on the player's own thread, so the first pass at a new
    /// playhead position usually finds nothing ready — which is the design, not
    /// a fault: the render path never waits.
    #[cfg(unix)]
    fn pixel_at(
        playback: &mut Playback,
        state: &ShowState,
        catalogue: &MediaCatalogue,
        compositor: &mut HeadlessCompositor,
        layer_id: LayerId,
        now: Instant,
    ) -> [u8; 4] {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let report = playback.update(state, catalogue, compositor, now, a_wall());
            if report.uploaded > 0 {
                if let Some((_, pixel)) = compositor.texture(layer_id) {
                    return pixel;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("no frame reached the GPU within five seconds");
    }

    /// Whether a pixel is the colour it was encoded as.
    ///
    /// Generous, because the fixture went through 10-bit 4:2:2 YUV and comes
    /// back through a colour conversion, and neither trip is lossless. It is
    /// still far tighter than the difference between red and green.
    #[cfg(unix)]
    #[track_caller]
    fn assert_colour(actual: [u8; 4], expected: [u8; 3], what: &str) {
        let close = |left: u8, right: u8| u16::from(left).abs_diff(u16::from(right)) <= 24;
        assert!(
            close(actual[0], expected[0])
                && close(actual[1], expected[1])
                && close(actual[2], expected[2]),
            "{what}: expected {expected:?}, got {:?}",
            &actual[..3]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_layer_reports_the_depth_its_own_clip_decodes_at() {
        // The owner's instruction of 2026-09-23, as a test on real files:
        //
        //   Source Decode: 10-bit / Composition: 8-bit / Output: 8-bit
        //
        // Three stages, three numbers, and the first of them belongs to the clip
        // rather than to the machine. The two committed fixtures happen to be
        // one of each — `colours.mov` is ProRes 422 Proxy, which decodes to
        // `yuv422p10le`, and `colours-h264.mp4` is `yuv420p` — so this needs no
        // generated material and it also writes down what those fixtures are.
        use ovs_types::depth::DecodeDepth;

        for (clip, wanted) in [(CLIP, DecodeDepth::Ten), (LONG_GOP, DecodeDepth::Eight)] {
            let item = MediaItemId::new();
            let (state, layer_id) = show(item);

            let mut catalogue = MediaCatalogue::new();
            catalogue.insert(
                item,
                MediaLocation {
                    path: PathBuf::from(clip),
                    dimensions: None,
                    duration_ms: None,
                    frame_rate: None,
                    ..location(item, MediaKind::Video)
                },
            );

            let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
            let start = Instant::now();
            let mut playback = Playback::new(start, false);
            let _ = pixel_at(
                &mut playback,
                &state,
                &catalogue,
                &mut compositor,
                layer_id,
                start,
            );

            let mut playheads = Vec::new();
            playback.playheads(&mut playheads);
            let head = playheads
                .iter()
                .find(|head| head.layer_id == layer_id)
                .expect("the layer that is playing has a playhead");
            assert_eq!(
                head.decode_depth, wanted,
                "{clip} should decode at {wanted:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_real_clip_plays_from_the_file() {
        // End to end with the decoder that ships: a file on disk, a slot, a
        // playhead, and the pixels that reach the GPU.
        //
        // The catalogue deliberately says nothing about how long the clip is or
        // what rate it runs at. That is the second thing this proves: the pass
        // is built from what the decoder found in the container, not from the
        // database row written at ingest. If the fallback were used the clip
        // would be four seconds long, and every colour below would be wrong.
        let item = MediaItemId::new();
        let (state, layer_id) = show(item);

        let mut catalogue = MediaCatalogue::new();
        catalogue.insert(
            item,
            MediaLocation {
                path: PathBuf::from(CLIP),
                dimensions: None,
                duration_ms: None,
                frame_rate: None,
                ..location(item, MediaKind::Video)
            },
        );

        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        // `false`: the decoder that ships, not a test's stand-in.
        let start = Instant::now();
        let mut playback = Playback::new(start, false);

        let opening = pixel_at(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            layer_id,
            start,
        );
        assert_colour(opening, [255, 0, 0], "the first frame");

        assert_eq!(
            compositor.texture(layer_id).map(|(size, _)| size),
            Some(SizePx::new(64, 64)),
            "at the clip's own size, not the catalogue's guess"
        );

        // Half way through 1.2 seconds of clip.
        let middle = pixel_at(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            layer_id,
            start + Duration::from_millis(600),
        );
        assert_colour(middle, [0, 255, 0], "half way through");

        // And five sixths of the way, which is the last third.
        let late = pixel_at(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            layer_id,
            start + Duration::from_millis(1_000),
        );
        assert_colour(late, [0, 0, 255], "near the end");
    }

    #[test]
    fn a_video_layer_gets_its_own_texture() {
        // Two layers playing the same file are at two different frames, so the
        // texture belongs to the layer rather than to the item.
        let item = MediaItemId::new();
        let (state, layer_id) = show(item);
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        let mut uploaded = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && uploaded == 0 {
            uploaded = playback
                .update(
                    &state,
                    &catalogue,
                    &mut compositor,
                    Instant::now(),
                    a_wall(),
                )
                .uploaded;
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(uploaded, 1, "one frame reached the GPU");
        assert!(
            compositor.texture(layer_id).is_some(),
            "and it is the layer's own texture"
        );
        assert!(
            compositor.texture(item).is_none(),
            "not the item's, which belongs to stills"
        );
        assert!(playback.showing().get(&layer_id) == Some(&true));
    }

    #[test]
    fn a_layer_that_stops_playing_gives_its_texture_back() {
        // A frame of something this layer is no longer showing is memory held
        // for nothing, and the wrong picture if the layer came back.
        let item = MediaItemId::new();
        let (mut state, layer_id) = show(item);
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && compositor.texture(layer_id).is_none() {
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                Instant::now(),
                a_wall(),
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(compositor.texture(layer_id).is_some(), "it was playing");

        // Slot 0 is Black: the layer is off stage.
        state.layers[0].active_slot = 0;
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert!(compositor.texture(layer_id).is_none(), "and now it is not");
        assert!(playback.showing().values().all(|showing| !*showing));
    }

    #[test]
    fn changing_a_slots_beats_or_rate_reaches_the_clip_that_is_already_playing() {
        // The owner, 2026-09-23: "Im Slot Speedscale und Beats a Pass ändern.
        // Es erfordert jedes mal ein restart des Slots. Das darf nicht sein.
        // Änderungen müssen sofort passieren."
        //
        // The pass used to be worked out once, when the slot was triggered, and
        // kept. So an operator correcting 7 beats to 54 on a clip that was
        // running saw nothing until they hit the pad again — and the fault that
        // sent them looking was a clip at twenty-one times speed, which is
        // exactly the moment somebody needs the number to change under them.
        let item = MediaItemId::new();
        let (mut state, layer_id) = show(item);
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        // Locked to eight beats. The trigger counter is noted, because the
        // whole point is that it must not move.
        let show = state.show.as_deref_mut().expect("a show");
        show.layers[0].slots[0].sync = ovs_types::config::SlotSync::BeatSync { beats: 8 };
        let trigger = state.layers[0].trigger;

        let start = Instant::now();
        playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        let before = playback
            .layers
            .get(&layer_id)
            .expect("playing")
            .triggered
            .pass;
        assert_eq!(before, Pass::Beats(8.0));

        // Two beats later the operator halves the rate. At 120 BPM that is a
        // second of wall clock, which is a real moment inside the pass rather
        // than the start of one.
        let show = state.show.as_deref_mut().expect("a show");
        show.layers[0].slots[0].speed = 0.5;
        let moment = start + Duration::from_millis(1_000);
        let was = playhead_of(&playback, layer_id, moment);
        playback.update(&state, &catalogue, &mut compositor, moment, a_wall());

        let playing = playback.layers.get(&layer_id).expect("playing");
        assert_eq!(
            playing.triggered.pass,
            Pass::Beats(16.0),
            "half speed is twice the beats, and it arrived without a retrigger"
        );
        assert_eq!(
            state.layers[0].trigger, trigger,
            "nothing retriggered the slot"
        );

        let now = playhead_of(&playback, layer_id, moment);
        assert!(
            (f64::from(now.position) - f64::from(was.position)).abs() < 1e-6,
            "the picture jumped: {} to {}",
            was.position,
            now.position
        );

        // And free run reaches it too, which is a change of clock as well.
        let show = state.show.as_deref_mut().expect("a show");
        show.layers[0].slots[0].sync = ovs_types::config::SlotSync::FreeRun;
        show.layers[0].slots[0].speed = 1.0;
        playback.update(&state, &catalogue, &mut compositor, moment, a_wall());
        assert!(
            matches!(
                playback
                    .layers
                    .get(&layer_id)
                    .expect("playing")
                    .triggered
                    .pass,
                Pass::Seconds(_)
            ),
            "the clip is on its own clock now"
        );
    }

    /// Where a layer's playhead is, without going through a whole pass.
    fn playhead_of(playback: &Playback, layer_id: LayerId, now: Instant) -> crate::media::Playhead {
        let playing = playback.layers.get(&layer_id).expect("playing");
        let clock = &playback.clock;
        let unit = match playing.triggered.pass {
            Pass::Seconds(_) => clock.seconds_at(now),
            Pass::Beats(_) => clock.beats_at(now),
        };
        playhead_at(
            unit,
            playing.triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        )
    }

    #[test]
    fn a_pad_just_pressed_opens_its_decoder_in_the_same_pass() {
        // Three tests used to live here, about a source opened *before* the
        // boundary a quantized trigger was waiting for. The owner removed the
        // trigger grid on 2026-09-23 — "Trigger Grid weg." — so there is no
        // before: the pass that sees the new slot is the pass that opens it.
        //
        // What is still worth pinning is that the layer shows nothing for that
        // one pass rather than something wrong. A quad sampling a texture that
        // is not there would draw white.
        let item = MediaItemId::new();
        let (state, layer_id) = show(item);
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );
        // **Whatever it managed in that pass, it does not claim more than it
        // has.** This used to insist on `false`, which is a statement about how
        // fast the decoder is rather than about what the scheduler reports —
        // and on 2026-09-27 a runner opened, decoded and uploaded a frame
        // inside that single call, which is not a fault.
        //
        // The agreement is the thing worth pinning, and it is what the note
        // above is about: a layer that says it is showing a frame it has not
        // uploaded draws a quad sampling a texture that is not there, and that
        // is white.
        assert_eq!(
            playback.showing().get(&layer_id).copied(),
            Some(compositor.texture(layer_id).is_some()),
            "the layer is this scheduler's, and says exactly what it has"
        );

        // And it arrives, without anything having had to be primed for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && playback.telemetry().playing == 0 {
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                Instant::now(),
                a_wall(),
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(playback.telemetry().playing, 1, "the clip is on stage");
        assert!(compositor.texture(layer_id).is_some());
    }

    /// A show whose layer holds a generator in slot 1.
    fn show_generating(source: SourceRef) -> (ShowState, LayerId) {
        let mut show = Show::new("Show");
        show.composition.size = SizePx::new(32, 32);
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot {
            index: 1,
            source,
            name: None,
            sync: ovs_types::config::slot::SlotSync::default(),
            speed: 1.0,
            playback_mode: ovs_types::config::PlaybackMode::Loop,
            direction: ovs_types::config::PlaybackDirection::Forward,
            transform: ovs_types::geometry::Transform2D::IDENTITY,
            effects: Vec::new(),
        });
        let layer_id = layer.id;
        show.layers.push(layer);

        let mut live = LayerLive::black(layer_id);
        live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![live],
            ..ShowState::default()
        };
        (state, layer_id)
    }

    fn solid(colour: [f32; 4]) -> SourceRef {
        SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::new(),
            type_id: "ovs.solid_colour".to_owned(),
            version: 1,
            parameters: ovs_types::config::ParameterMap::from([(
                "colour".to_owned(),
                ovs_types::config::ParameterValue::Color(colour),
            )]),
        }
    }

    /// A generator whose picture changes with time this test can move.
    ///
    /// A strobe at one hertz: its tick is the parity of the half-cycle, taken
    /// from how long the layer has been up, so half a second is a different
    /// picture and the test does not have to wait for the wall clock. Noise
    /// would read the wall clock in milliseconds, and two passes in a test are
    /// microseconds apart.
    fn strobe_at_one_hertz() -> SourceRef {
        SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::new(),
            type_id: "ovs.strobe".to_owned(),
            version: 1,
            parameters: ovs_types::config::ParameterMap::from([(
                "hz".to_owned(),
                ovs_types::config::ParameterValue::Float(1.0),
            )]),
        }
    }

    #[test]
    fn a_generator_on_a_layer_nobody_can_see_is_not_drawn() {
        // **Measured on the owner's machine, 2026-10-14.** With *every* layer
        // disabled the wall still ran at 15.6 fps and `draw_kind` was still two
        // thirds of the render thread: a full-size picture, on the processor,
        // sixty times a second, for layers that were switched off.
        //
        // A moving generator, because a still one is drawn once and left alone
        // anyway; this is about the redraw.
        let (mut state, _layer_id) = show_generating(strobe_at_one_hertz());
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = MediaCatalogue::new();
        let start = Instant::now();

        // **Live at the desk**, which is the owner's case: the layer was
        // switched off in the document while nobody had muted it. The old rule
        // left the player running for exactly that pair, so this is the one
        // arrangement where the drawing had to be stopped and was not.
        state.layers[0].enabled = true;

        let first = playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        assert_eq!(first.uploaded, 1, "drawn while it is on stage");

        // Switched off in the document, which is one of the two ways a layer
        // leaves the wall.
        state.show.as_mut().expect("a show").layers[0].enabled = false;
        let hidden = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(500),
            a_wall(),
        );
        assert_eq!(hidden.uploaded, 0, "not drawn while nobody can see it");

        // And it comes straight back: a generator's picture is a function of
        // its tick, so there is nothing to catch up on.
        state.show.as_mut().expect("a show").layers[0].enabled = true;
        let back = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            // A different half-cycle from both passes before it: at one hertz
            // 1.0 s is the same parity as 0.0 s, and a test that asked for the
            // picture it had already drawn would prove nothing either way.
            start + Duration::from_millis(1_500),
            a_wall(),
        );
        assert_eq!(back.uploaded, 1, "drawn again the pass it returns");
    }

    #[test]
    fn a_generator_is_drawn_once_and_then_left_alone() {
        // The picture is identical every frame, so filling it sixty times a
        // second would be work with no output.
        let (state, layer_id) = show_generating(solid([1.0, 0.0, 0.0, 1.0]));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = MediaCatalogue::new();

        let first = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );
        assert_eq!(first.uploaded, 1, "drawn");
        assert_eq!(first.playing, 1);
        assert_eq!(
            compositor.texture(layer_id).map(|(_, pixel)| pixel),
            Some([255, 0, 0, 255])
        );

        let second = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );
        assert_eq!(second.uploaded, 0, "and not drawn again");
        assert_eq!(second.playing, 1, "but still on stage");
        assert!(playback.showing().get(&layer_id) == Some(&true));
    }

    #[test]
    fn changing_what_a_generator_draws_redraws_it() {
        // An operator editing the colour, which is the whole reason a generator
        // has parameters at all.
        let (mut state, layer_id) = show_generating(solid([1.0, 0.0, 0.0, 1.0]));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = MediaCatalogue::new();
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        // The same layer and the same slot, edited — which is what an operator
        // changing a colour actually does.
        state.show.as_mut().expect("a show").layers[0].slots[0].source =
            solid([0.0, 0.0, 1.0, 1.0]);
        let report = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(report.uploaded, 1);
        assert_eq!(
            compositor.texture(layer_id).map(|(_, pixel)| pixel),
            Some([0, 0, 255, 255])
        );
    }

    #[test]
    fn a_generator_this_build_does_not_publish_leaves_the_layer_on_its_placeholder() {
        // A show from another build. Better nothing than something that
        // looks deliberate.
        let (state, layer_id) = show_generating(SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::new(),
            type_id: "third.party.plasma".to_owned(),
            version: 1,
            parameters: ovs_types::config::ParameterMap::new(),
        });
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);

        let report = playback.update(
            &state,
            &MediaCatalogue::new(),
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(report.failed, 1);
        assert_eq!(report.playing, 0);
        assert_eq!(playback.showing().get(&layer_id), Some(&false));
        assert!(compositor.texture(layer_id).is_none());
    }

    #[test]
    fn a_layer_that_stops_generating_gives_its_texture_back() {
        let (mut state, layer_id) = show_generating(solid([0.0, 1.0, 0.0, 1.0]));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = MediaCatalogue::new();

        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );
        assert!(compositor.texture(layer_id).is_some());

        // Slot 0 is Black: the layer is off stage.
        state.layers[0].active_slot = 0;
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert!(compositor.texture(layer_id).is_none());
        assert_eq!(playback.telemetry().playing, 0);
    }

    #[test]
    fn a_generator_fills_the_composition_rather_than_guessing_a_size() {
        // A generator has no natural size; the composition is what it is drawn
        // for, and a wall is the size the show says.
        let (state, layer_id) = show_generating(solid([1.0, 1.0, 1.0, 1.0]));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);

        playback.update(
            &state,
            &MediaCatalogue::new(),
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(
            compositor.texture(layer_id).map(|(size, _)| size),
            Some(SizePx::new(32, 32))
        );
    }

    /// A countdown that either does or does not restart when the pad is hit.
    fn countdown(from_trigger: bool) -> SourceRef {
        SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::new(),
            type_id: "ovs.countdown".to_owned(),
            version: 1,
            parameters: ovs_types::config::ParameterMap::from([(
                "from_trigger".to_owned(),
                ovs_types::config::ParameterValue::Bool(from_trigger),
            )]),
        }
    }

    #[test]
    fn a_countdown_is_drawn_again_when_its_second_moves_and_not_otherwise() {
        // The first generator whose picture is a function of time as well as
        // position. Drawing it every pass would be sixty compositions a second
        // for a number that changes once; not drawing it again at all would be
        // a photograph of a stage clock.
        let (state, _) = show_generating(countdown(true));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let start = Instant::now();
        let mut playback = Playback::new(start, false);
        let catalogue = MediaCatalogue::new();

        let first = playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        assert_eq!(first.uploaded, 1, "drawn");
        assert_eq!(first.playing, 1);

        let same_second = playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        assert_eq!(same_second.uploaded, 0, "and not drawn again for nothing");
        assert_eq!(same_second.playing, 1, "but still on stage");

        let next_second = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(1),
            a_wall(),
        );
        assert_eq!(next_second.uploaded, 1, "and the next second is drawn");
    }

    #[test]
    fn retriggering_a_countdown_starts_it_again_only_when_it_was_asked_to() {
        // A speaker's clock is reset between talks, so the pad restarts it. One
        // running against something in the world must not be disturbed by an
        // operator bringing it up on a screen, so the pad does nothing to it.
        let run = |from_trigger: bool| {
            let (mut state, _) = show_generating(countdown(from_trigger));
            let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
            let start = Instant::now();
            let mut playback = Playback::new(start, false);
            let catalogue = MediaCatalogue::new();

            playback.update(&state, &catalogue, &mut compositor, start, a_wall());
            let five_seconds_in = start + Duration::from_secs(5);
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                five_seconds_in,
                a_wall(),
            );

            // The pad, and nothing else about the slot changed.
            state.layers[0].trigger += 1;
            playback
                .update(
                    &state,
                    &catalogue,
                    &mut compositor,
                    five_seconds_in,
                    a_wall(),
                )
                .uploaded
        };

        assert_eq!(run(true), 1, "back to the start, so the picture changed");
        assert_eq!(run(false), 0, "and here it carried on undisturbed");
    }

    #[test]
    fn a_clock_layer_is_drawn_and_stays_on_stage() {
        // A clock is on the venue's wall clock rather than on anything this test
        // can move, so what is asserted is that it reaches the GPU and stays
        // there. Which second it draws is the generator's own test.
        let (state, layer_id) = show_generating(SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::new(),
            type_id: "ovs.clock".to_owned(),
            version: 1,
            parameters: ovs_types::config::ParameterMap::new(),
        });
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let start = Instant::now();
        let mut playback = Playback::new(start, false);
        let catalogue = MediaCatalogue::new();

        let first = playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        assert_eq!(first.uploaded, 1);
        assert_eq!(first.playing, 1);
        assert!(compositor.texture(layer_id).is_some());

        let again = playback.update(&state, &catalogue, &mut compositor, start, a_wall());
        assert_eq!(again.playing, 1, "still on stage");
        assert!(playback.showing().get(&layer_id) == Some(&true));
    }

    /// A show playing `item` in the given mode and direction.
    /// One layer with a generator on slot 1 and a clip on slot 2.
    fn show_two_slots(media_item_id: MediaItemId, generator: SourceRef) -> (ShowState, LayerId) {
        let mut show = Show::new("Show");
        show.composition.size = SizePx::new(32, 32);
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot {
            index: 1,
            source: generator,
            name: None,
            sync: ovs_types::config::slot::SlotSync::default(),
            speed: 1.0,
            playback_mode: ovs_types::config::PlaybackMode::Loop,
            direction: ovs_types::config::PlaybackDirection::Forward,
            transform: ovs_types::geometry::Transform2D::IDENTITY,
            effects: Vec::new(),
        });
        layer.slots.push(Slot::media(2, media_item_id));
        let layer_id = layer.id;
        show.layers.push(layer);

        let mut live = LayerLive::black(layer_id);
        live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![live],
            ..ShowState::default()
        };
        (state, layer_id)
    }

    #[test]
    fn switching_back_to_a_generator_draws_it_again() {
        // The owner, from the desk on 2026-10-10: "Slot3 NDI und Slot1+2
        // Generator.. es passiert nur wenn ich zwischen Slot 1 und Slot3 bzw.
        // Slot 2 und Slot3 wechsle hin und her". What a generator was built
        // from includes its slot, so going back to the slot it was last built
        // for leaves that unchanged and it decides it has already drawn --
        // while something else has had the layer's texture in between, and
        // that is what stays on the wall. Rotating through three slots changes
        // it every time, which is why "Slot 3 > Slot 2 > Slot 3 > Slot 1"
        // looked perfectly fine.
        //
        // A clip stands in for the network source he found it with. Both write
        // `TextureKey::Layer` and the fault is in that sharing, not in either
        // of them; NDI needs a runtime a test cannot have, and this is the
        // half he had already seen it in: "Das hatte ich aber auch schon mit
        // Videos das Phaenomen".
        let item = MediaItemId::new();
        let (mut state, layer_id) = show_two_slots(item, solid([1.0, 0.0, 0.0, 1.0]));
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), true);
        let red = Some([255, 0, 0, 255]);

        let first = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );
        assert_eq!(first.uploaded, 1, "the generator drew itself");
        assert_eq!(compositor.texture(layer_id).map(|(_, pixel)| pixel), red);

        // On to the clip, and wait for a frame of it to land: the decoder is on
        // its own thread, so the pass that switches has nothing to show yet.
        state.layers[0].active_slot = 2;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && compositor.texture(layer_id).map(|(_, pixel)| pixel) == red
        {
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                Instant::now(),
                a_wall(),
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_ne!(
            compositor.texture(layer_id).map(|(_, pixel)| pixel),
            red,
            "the clip has the layer's texture now"
        );

        // And back to the generator that has already drawn once.
        state.layers[0].active_slot = 1;
        let back = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(
            back.uploaded, 1,
            "drawn again, because the texture moved on"
        );
        assert_eq!(
            compositor.texture(layer_id).map(|(_, pixel)| pixel),
            red,
            "and the wall shows the generator rather than the clip"
        );
    }

    fn show_playing(
        media_item_id: MediaItemId,
        mode: ovs_types::config::PlaybackMode,
        direction: ovs_types::config::PlaybackDirection,
    ) -> (ShowState, LayerId) {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot {
            index: 1,
            source: SourceRef::Media { media_item_id },
            name: None,
            sync: ovs_types::config::slot::SlotSync::default(),
            speed: 1.0,
            playback_mode: mode,
            direction,
            transform: ovs_types::geometry::Transform2D::IDENTITY,
            effects: Vec::new(),
        });
        let layer_id = layer.id;
        show.layers.push(layer);

        let mut live = LayerLive::black(layer_id);
        live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![live],
            ..ShowState::default()
        };
        (state, layer_id)
    }

    /// The frame index on the GPU, read from the pixel the source stamps it in.
    fn frame_on_screen(compositor: &HeadlessCompositor, layer_id: LayerId) -> Option<u32> {
        compositor
            .texture(layer_id)
            .map(|(_, pixel)| u32::from(pixel[0]) | (u32::from(pixel[1]) << 8))
    }

    /// Runs the scheduler at a moment and returns the frame that settles there.
    ///
    /// The player decodes on its own thread — which is the whole point of it —
    /// so the first answer after a jump is usually the frame it was already
    /// holding. This keeps asking at the same moment until the answer stops
    /// changing, which is what the render loop does at sixty frames a second
    /// anyway.
    fn at_time(
        playback: &mut Playback,
        state: &ShowState,
        catalogue: &MediaCatalogue,
        compositor: &mut HeadlessCompositor,
        start: Instant,
        seconds: f64,
    ) -> Option<u32> {
        let now = start + Duration::from_secs_f64(seconds);
        let layer_id = LAYER.with(|layer| *layer.borrow());

        // Waits for a signal rather than for quiet.
        //
        // A player that has not been scheduled is still holding the previous
        // pass's frame, and holding it steadily — which is indistinguishable
        // from an answer if all a test does is watch for the picture to stop
        // changing. Counting identical readings was tried at ten, forty and
        // sixty in a row and failed under a loaded machine every time, because
        // a stall is exactly as quiet as an answer.
        //
        // So this asks the direct question instead: is the picture on the
        // output the frame the playhead is on? The playhead is worked out from
        // the moment, the picture is read back off the GPU, and when they agree
        // the decoder has caught up. Nothing here depends on how a counter is
        // kept, which is what an earlier version of this did — it watched
        // `late` stop rising, and that stopped being a signal the moment a
        // missed frame was counted once instead of once per pass.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut playheads = Vec::new();
        let mut settled = None;

        while Instant::now() < deadline {
            playback.update(state, catalogue, compositor, now, a_wall());
            playback.playheads(&mut playheads);

            let wanted = playheads
                .iter()
                .find(|playhead| playhead.layer_id == layer_id)
                .map(|playhead| playhead.frame);
            let seen = frame_on_screen(compositor, layer_id);
            settled = seen;

            if let (Some(wanted), Some(seen)) = (wanted, seen) {
                if wanted == seen {
                    return Some(seen);
                }
            }

            // Give the decoder's thread the processor rather than only sleeping
            // on it: under load it is the one thing this is waiting for.
            std::thread::yield_now();
            std::thread::sleep(Duration::from_millis(2));
        }

        settled
    }

    thread_local! {
        /// The layer the helpers above are looking at.
        static LAYER: std::cell::RefCell<LayerId> = std::cell::RefCell::new(LayerId::new());
    }

    #[test]
    fn a_looping_clip_comes_back_round() {
        // Four seconds at 120 BPM is eight beats, so half a pass is two
        // seconds and a whole pass brings it back to the start.
        //
        // The readings are taken a little way past each landmark rather than on
        // it. A pass boundary is one instant where "the end of this pass" and
        // "the start of the next" are the same moment, and which of the two a
        // playhead reports there comes down to the last bit of a division —
        // this asked at exactly one pass and got frame 99 about one run in
        // three. That is the question being badly put, not the playhead being
        // wrong.
        let item = MediaItemId::new();
        let (state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        LAYER.with(|layer| *layer.borrow_mut() = layer_id);

        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);
        let start = Instant::now();

        let first = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            0.05,
        )
        .expect("a frame");
        let middle = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            2.0,
        )
        .expect("a frame");
        let round = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            4.25,
        )
        .expect("a frame");

        assert!(first < 10, "it starts at the beginning: {first}");
        assert!(
            (40..=60).contains(&middle),
            "halfway through a hundred frames: {middle}"
        );
        assert!(round < 10, "and a whole pass brings it back: {round}");
    }

    #[test]
    fn a_bouncing_clip_turns_round_rather_than_jumping_back() {
        // What Bounce means on a wall: the picture reverses at the end instead
        // of cutting to the first frame. The difference is the whole feature.
        let item = MediaItemId::new();
        let (state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Bounce,
            ovs_types::config::PlaybackDirection::Forward,
        );
        LAYER.with(|layer| *layer.borrow_mut() = layer_id);

        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);
        let start = Instant::now();

        // The pass starts when the scheduler first sees the layer, so this is
        // where the clip is triggered from. Everything after is measured from
        // here.
        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            0.0,
        )
        .expect("a frame");

        // One pass out and one pass back, so a four-second clip turns round at
        // four seconds: three-quarters through is frame 75 on the way out and
        // frame 25 on the way back.
        let early = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            1.0,
        )
        .expect("a frame");
        let far = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            3.0,
        )
        .expect("a frame");
        let returning = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            7.0,
        )
        .expect("a frame");

        assert!(early < far, "out: {early} then {far}");
        assert!(returning < far, "and back again: {far} then {returning}");
    }

    #[test]
    fn a_clip_that_plays_once_and_blacks_takes_the_layer_off_stage() {
        // Once→Black is the mode that ends. Anything else holds a frame, and
        // the difference matters to whoever built the show around it.
        let item = MediaItemId::new();
        let (state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::OnceBlack,
            ovs_types::config::PlaybackDirection::Forward,
        );
        LAYER.with(|layer| *layer.borrow_mut() = layer_id);

        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);
        let start = Instant::now();

        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            0.0,
        )
        .expect("a frame");
        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            1.0,
        )
        .expect("a frame");

        // Well past the end of a four-second pass.
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(10),
            a_wall(),
        );
        assert_eq!(
            playback.showing().get(&layer_id),
            Some(&false),
            "the layer is off stage"
        );
    }

    #[test]
    fn a_clip_that_plays_once_and_freezes_holds_its_last_frame() {
        // The other end of the same decision: a held frame is a frame, so the
        // layer stays on stage showing it.
        let item = MediaItemId::new();
        let (state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::OnceFreeze,
            ovs_types::config::PlaybackDirection::Forward,
        );
        LAYER.with(|layer| *layer.borrow_mut() = layer_id);

        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);
        let start = Instant::now();

        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            0.0,
        )
        .expect("a frame");
        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            1.0,
        )
        .expect("a frame");

        let held = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            10.0,
        )
        .expect("a frame");

        assert_eq!(
            playback.showing().get(&layer_id),
            Some(&true),
            "still on stage"
        );
        assert!(held > 90, "and holding the end of the clip: {held}");
    }

    // Two tests stood here, and what they tested no longer exists.
    //
    // They asked what halving a *layer's* rate does to a clip: how far it had
    // travelled, and what rate a client's progress bar should animate at. A
    // layer has no rate any more. What a clip does is the slot's to say —
    // free run at its own speed, or a number of beats — and the owner's reason
    // is that how long a piece of footage takes is a fact about the footage
    // rather than about the layer it happens to be on (2026-09-23).
    //
    // The behaviour they were protecting is covered where it now lives:
    // `pass_for` in `media/timeline.rs` for what a slot's setting means, and
    // `a_free_running_clip_ignores_the_tempo` below for the half that used to
    // be impossible to state.

    #[test]
    fn retriggering_starts_the_pass_again() {
        // An operator hitting the same pad. The slot did not change, so only
        // the trigger counter says it happened — and the picture has to go
        // back to the beginning.
        let item = MediaItemId::new();
        let (mut state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        LAYER.with(|layer| *layer.borrow_mut() = layer_id);

        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);
        let start = Instant::now();

        at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            0.0,
        )
        .expect("a frame");
        let along = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            2.0,
        )
        .expect("a frame");
        assert!(along > 30, "well into the clip: {along}");

        state.layers[0].trigger += 1;
        let restarted = at_time(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start,
            2.05,
        )
        .expect("a frame");

        assert!(restarted < 10, "back at the beginning: {restarted}");
    }

    #[test]
    fn a_clips_length_in_beats_comes_from_the_tempo_it_was_triggered_at() {
        // Four seconds at 120 BPM is eight beats, and stays eight beats
        // afterwards: that is what locks a clip to the music.
        let location = location(MediaItemId::new(), MediaKind::Video);
        let length = length_of(&location, true);
        assert_eq!(length.duration_ms, 4_000);
        assert_eq!(length.frame_count, 100, "four seconds at 25 fps");
    }

    #[test]
    fn a_clip_the_catalogue_cannot_measure_still_gets_a_timeline() {
        // The catalogue's numbers come from a database that can be out of date.
        // Refusing to play would turn a stale row into a black wall.
        let mut location = location(MediaItemId::new(), MediaKind::Video);
        location.duration_ms = None;
        location.frame_rate = None;

        let length = length_of(&location, true);
        assert!(length.duration_ms > 0);
        assert!(length.frame_count > 1);
    }

    /// Runs the scheduler at one moment until the layer reports a playhead.
    ///
    /// The decode is on the player's own thread, so the first pass at a new
    /// moment usually finds nothing on stage yet — and a layer with nothing on
    /// stage deliberately has no playhead.
    fn playhead_reported(
        playback: &mut Playback,
        state: &ShowState,
        catalogue: &MediaCatalogue,
        compositor: &mut HeadlessCompositor,
        at: Instant,
    ) -> Option<ovs_types::telemetry::LayerPlayhead> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reported = Vec::new();
        while Instant::now() < deadline {
            playback.update(state, catalogue, compositor, at, a_wall());
            playback.playheads(&mut reported);
            if let Some(head) = reported.first() {
                return Some(*head);
            }
            std::thread::yield_now();
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    /// What the scheduler would report right now, without waiting for anything.
    fn reported(playback: &Playback) -> Vec<ovs_types::telemetry::LayerPlayhead> {
        let mut into = Vec::new();
        playback.playheads(&mut into);
        into
    }

    #[test]
    fn a_layer_playing_a_clip_says_where_its_playhead_is() {
        // The thing an operator cannot see today: how far through the clip on
        // the pad they are about to cue away from actually is.
        let item = MediaItemId::new();
        let (state, layer_id) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        // The pass is triggered on the first update the scheduler makes, so it
        // starts at `start` and the reading below is taken a second into it.
        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(1),
            a_wall(),
        );
        let head = reported(&playback)
            .first()
            .copied()
            .expect("a layer playing a clip has a playhead");

        assert_eq!(head.layer_id, layer_id);
        assert_eq!(head.media_item_id, item, "and says which clip it is of");
        assert_eq!(head.slot, 1);

        // Four seconds at 25 fps, which is what the catalogue says.
        assert_eq!(head.duration_ms, 4_000);
        assert_eq!(head.frame_count, 100);

        assert!(
            head.frame < head.frame_count,
            "the playhead is inside the clip: frame {} of {}",
            head.frame,
            head.frame_count
        );
        assert!(
            head.position_ms <= head.duration_ms,
            "and so is the time: {} of {}",
            head.position_ms,
            head.duration_ms
        );
        assert!(
            head.position_ms > 0,
            "a second in, it has moved off the first frame"
        );

        // The two units agree: an operator reading seconds and a scheduler
        // reading frames must be looking at the same place.
        let through = head.fraction().expect("a clip with a length");
        let from_frames = head.frame as f32 / head.frame_count as f32;
        assert!(
            (through - from_frames).abs() < 0.02,
            "frames say {from_frames}, milliseconds say {through}"
        );
    }

    #[test]
    fn a_layer_showing_a_generator_has_no_playhead() {
        // A generator draws itself. There is no timeline to be part of the way
        // through, so there is nothing to draw a bar of — and a bar at zero
        // would say a clip had just started.
        let (state, _) = show_generating(solid([1.0, 0.0, 0.0, 1.0]));
        let mut compositor = HeadlessCompositor::sized(SizePx::new(32, 32));
        let mut playback = Playback::new(Instant::now(), false);
        let catalogue = MediaCatalogue::new();

        let report = playback.update(
            &state,
            &catalogue,
            &mut compositor,
            Instant::now(),
            a_wall(),
        );

        assert_eq!(report.playing, 1, "it is very much on stage");
        assert!(
            reported(&playback).is_empty(),
            "but it has no playhead to report"
        );
    }

    #[test]
    fn a_layer_showing_nothing_has_no_playhead() {
        // Slot 0 is permanently black. The playhead must go with the clip
        // rather than being left behind at wherever it stopped.
        let item = MediaItemId::new();
        let (mut state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");

        state.layers[0].active_slot = 0;
        playback.update(&state, &catalogue, &mut compositor, start, a_wall());

        assert!(
            reported(&playback).is_empty(),
            "a layer on black has no playhead"
        );
    }

    #[test]
    fn a_clip_that_has_played_once_and_blacked_leaves_no_playhead_behind() {
        // The other way a layer ends up showing nothing. A bar left frozen at
        // the end would say a clip was still up.
        let item = MediaItemId::new();
        let (state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::OnceBlack,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");

        // Well past the end of a four-second pass.
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(10),
            a_wall(),
        );

        assert!(reported(&playback).is_empty());
    }

    #[test]
    fn a_paused_layer_holds_the_frame_it_was_showing() {
        // The thing slot 0 cannot do (owner decision Q16). Coming back from
        // black restarts the clip; coming back from pause carries on.
        let item = MediaItemId::new();
        let (mut state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");

        // A second in, and then paused.
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(1),
            a_wall(),
        );
        state.layers[0].transport = Transport::Paused;
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(1_100),
            a_wall(),
        );
        let paused = reported(&playback).first().copied().expect("a playhead");

        // Two more seconds of music go by and the playhead does not move.
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(3),
            a_wall(),
        );
        let still = reported(&playback).first().copied().expect("a playhead");
        assert_eq!(
            still.frame, paused.frame,
            "a paused layer held {} and then {}",
            paused.frame, still.frame
        );

        // And resuming carries on from there rather than from where the music
        // got to: about a second in, not about three.
        state.layers[0].transport = Transport::Playing;
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(3_100),
            a_wall(),
        );
        let resumed = reported(&playback).first().copied().expect("a playhead");
        assert!(
            resumed.position_ms >= still.position_ms
                && resumed.position_ms < still.position_ms + 400,
            "resumed at {} having paused at {}",
            resumed.position_ms,
            still.position_ms
        );
    }

    #[test]
    fn a_stopped_layer_goes_back_to_the_beginning_and_stays_there() {
        // Stop is not pause: the playhead rewinds, and resuming starts the clip
        // rather than dropping into the middle of it.
        let item = MediaItemId::new();
        let (mut state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_secs(2),
            a_wall(),
        );
        let running = reported(&playback).first().copied().expect("a playhead");
        assert!(running.position_ms > 500, "it had got somewhere first");

        state.layers[0].transport = Transport::Stopped;
        for after in [2_100, 4_000] {
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                start + Duration::from_millis(after),
                a_wall(),
            );
            let stopped = reported(&playback).first().copied().expect("a playhead");
            assert_eq!(stopped.frame, 0, "stopped at {after} ms");
            assert!(stopped.position_ms < 100, "stopped at {after} ms");
        }
    }

    #[test]
    fn a_muted_layer_keeps_running_where_it_was() {
        // The owner, 2026-09-23: "Mute ist nicht mehr sichtbar, aber soll
        // weiterlaufen. Aktuell starten die Slots jedes mal neu."
        //
        // MUTE is what an operator presses to see what is under a layer for two
        // bars. A clip that came back from its first frame afterwards is a clip
        // that has lost the show — so the player stays, and only the drawing
        // stops (which is `scene`'s, and is tested there).
        let item = MediaItemId::new();
        let (mut state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");
        assert_eq!(playback.telemetry().playing, 1);

        state.layers[0].enabled = false;
        let muted = playhead_reported(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(100),
        )
        .expect("it is still playing");
        assert_eq!(playback.telemetry().playing, 1, "the player stayed");

        // And it did not go back to the top while it was muted.
        state.layers[0].enabled = true;
        let after = playhead_reported(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(400),
        )
        .expect("it came back");
        assert_eq!(playback.telemetry().playing, 1);
        assert!(
            after.position_ms > muted.position_ms,
            "it carried on: {} then {}",
            muted.position_ms,
            after.position_ms
        );
    }

    #[test]
    fn a_layer_out_of_the_composition_both_ways_stops_costing_anything() {
        // The one case where nothing at the desk can bring the layer back, so
        // the decoder and its thread are worth saving: out of the composition in
        // the document *and* muted live (owner decision Q16).
        let item = MediaItemId::new();
        let (mut state, _) = show_playing(
            item,
            ovs_types::config::PlaybackMode::Loop,
            ovs_types::config::PlaybackDirection::Forward,
        );
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        let mut playback = Playback::new(start, true);

        playhead_reported(&mut playback, &state, &catalogue, &mut compositor, start)
            .expect("it was playing");
        assert_eq!(playback.telemetry().playing, 1);

        if let Some(show) = state.show.as_deref_mut() {
            show.layers[0].enabled = false;
        }
        state.layers[0].enabled = false;
        playback.update(
            &state,
            &catalogue,
            &mut compositor,
            start + Duration::from_millis(100),
            a_wall(),
        );
        assert_eq!(playback.telemetry().playing, 0, "the player is retired");
        assert!(reported(&playback).is_empty(), "and so is its playhead");
    }

    #[test]
    fn the_master_speed_changes_how_fast_a_playhead_moves_too() {
        // Two controls move a playhead and a client has them in two different
        // messages. Sending the product is what stops a screen multiplying the
        // wrong pair.
        let triggered = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };

        // 120 BPM at unity is two beats a second.
        let normal = playhead_speed(
            triggered,
            4_000,
            2.0,
            2.0,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        // The master speed doubled, which doubles the beat rate.
        let faster = playhead_speed(
            triggered,
            4_000,
            2.0,
            4.0,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        assert!((normal - 1_000.0).abs() < 1e-6, "got {normal}");
        assert!((faster - 2_000.0).abs() < 1e-6, "got {faster}");
    }

    #[test]
    fn a_playhead_running_backwards_reports_a_speed_that_says_so() {
        // A bar animated forwards over a clip that is running back would be
        // wrong for as long as the sample lasted, which is most of a second.
        let triggered = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };
        let speed =
            |mode, direction, at| playhead_speed(triggered, 4_000, at, 2.0, mode, direction);

        assert!(
            speed(PlaybackMode::Loop, PlaybackDirection::Forward, 2.0) > 0.0,
            "forwards"
        );
        assert!(
            speed(PlaybackMode::Loop, PlaybackDirection::Reverse, 2.0) < 0.0,
            "a reversed slot runs the timeline backwards"
        );
        assert!(
            speed(PlaybackMode::Bounce, PlaybackDirection::Forward, 2.0) > 0.0,
            "the way out of a bounce"
        );
        assert!(
            speed(PlaybackMode::Bounce, PlaybackDirection::Forward, 10.0) < 0.0,
            "and the way back"
        );
        assert!(
            speed(PlaybackMode::Bounce, PlaybackDirection::Reverse, 10.0) > 0.0,
            "reversed and bouncing back is forwards again"
        );
    }

    #[test]
    fn a_frozen_clip_reports_a_playhead_that_has_stopped() {
        // Once→Freeze holds its last frame. A bar that kept moving over it
        // would be an animation of something that has stopped.
        let triggered = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };

        let playing = playhead_speed(
            triggered,
            4_000,
            2.0,
            2.0,
            PlaybackMode::OnceFreeze,
            PlaybackDirection::Forward,
        );
        let frozen = playhead_speed(
            triggered,
            4_000,
            10.0,
            2.0,
            PlaybackMode::OnceFreeze,
            PlaybackDirection::Forward,
        );

        assert!((playing - 1_000.0).abs() < 1e-6, "got {playing}");
        assert!(frozen.abs() < f64::EPSILON, "got {frozen}");
    }

    #[test]
    fn a_pass_that_has_not_started_yet_is_not_moving() {
        // It holds its first frame, so a bar for it must sit still rather than
        // animate over something that is not moving.
        let waiting = Triggered {
            started_at: 8.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };
        let speed = playhead_speed(
            waiting,
            4_000,
            4.0,
            2.0,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        assert!(speed.abs() < f64::EPSILON, "got {speed}");
    }

    #[test]
    fn the_reported_speed_matches_where_the_playhead_actually_goes() {
        // The rate is worked out in closed form rather than by watching the
        // playhead move, so this is what keeps the two from drifting apart: a
        // fifth of a second later, the playhead is where the speed said it
        // would be, in every mode that has a direction of its own.
        const DURATION_MS: u64 = 4_000;
        const BEATS_PER_SECOND: f64 = 2.0;
        const STEP_SECONDS: f64 = 0.2;

        let triggered = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };

        for (mode, direction, at_beats, what) in [
            (
                PlaybackMode::Loop,
                PlaybackDirection::Forward,
                2.0,
                "a loop",
            ),
            (
                PlaybackMode::Loop,
                PlaybackDirection::Reverse,
                2.0,
                "a reversed loop",
            ),
            (
                PlaybackMode::Bounce,
                PlaybackDirection::Forward,
                2.0,
                "a bounce on the way out",
            ),
            (
                PlaybackMode::Bounce,
                PlaybackDirection::Forward,
                10.0,
                "a bounce on the way back",
            ),
            (
                PlaybackMode::OnceFreeze,
                PlaybackDirection::Forward,
                2.0,
                "a clip playing once",
            ),
            (
                PlaybackMode::OnceFreeze,
                PlaybackDirection::Forward,
                10.0,
                "the same clip, frozen",
            ),
        ] {
            let claimed = playhead_speed(
                triggered,
                DURATION_MS,
                at_beats,
                BEATS_PER_SECOND,
                mode,
                direction,
            );

            let place = |beats: f64| {
                let head = playhead_at(beats, triggered, mode, direction);
                position_ms(f64::from(head.position), DURATION_MS) as f64
            };
            let later = at_beats + BEATS_PER_SECOND * STEP_SECONDS;
            let measured = (place(later) - place(at_beats)) / STEP_SECONDS;

            assert!(
                (measured - claimed).abs() < 15.0,
                "{what}: it said {claimed} ms/s and moved at {measured} ms/s"
            );
        }
    }

    #[test]
    fn forgetting_everything_takes_the_textures_with_it() {
        // What a show change does. A layer of a show that has been closed
        // has no business keeping a frame of it on the GPU.
        let item = MediaItemId::new();
        let (state, layer_id) = show(item);
        let catalogue = catalogue(item, MediaKind::Video);
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let mut playback = Playback::new(Instant::now(), true);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && compositor.texture(layer_id).is_none() {
            playback.update(
                &state,
                &catalogue,
                &mut compositor,
                Instant::now(),
                a_wall(),
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        playback.forget_everything(&mut compositor);
        assert!(compositor.texture(layer_id).is_none());
        assert!(playback.showing().values().all(|showing| !*showing));
    }

    /// A show with one layer and two slots, each on its own real clip.
    ///
    /// Two items rather than one: retriggering the same item keeps its player on
    /// purpose, and a changeover is exactly the case where it cannot.
    #[cfg(unix)]
    fn show_with_two_clips() -> (ShowState, LayerId, MediaCatalogue) {
        let first = MediaItemId::new();
        let second = MediaItemId::new();

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, first));
        layer.slots.push(Slot::media(2, second));
        let layer_id = layer.id;
        show.layers.push(layer);

        let mut live = LayerLive::black(layer_id);
        live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![live],
            ..ShowState::default()
        };

        // Nothing said about length or rate, so each pass is built from what the
        // decoder finds in the container rather than from a database row.
        let mut catalogue = MediaCatalogue::new();
        for (item, path) in [(first, LONG_GOP), (second, CLIP)] {
            catalogue.insert(
                item,
                MediaLocation {
                    path: PathBuf::from(path),
                    dimensions: None,
                    duration_ms: None,
                    frame_rate: None,
                    ..location(item, MediaKind::Video)
                },
            );
        }
        (state, layer_id, catalogue)
    }

    #[cfg(unix)]
    #[test]
    fn a_changeover_never_costs_the_thread_that_makes_it_a_frame() {
        // **The owner's fault, as a number** (owner, 2026-09-25: "Es passiert
        // offensichtlich nur beim Starten der Medien").
        //
        // Measured on the reference machine before this: opening H.264 on the
        // card cost the thread 78 ms and letting go of it 275, so a changeover
        // made on the render thread was about twenty-one frames of a sixty hertz
        // wall (`crate::media::changeover`).
        //
        // What is asserted is not "the open is on a worker" — that is an
        // implementation, and a test of it would pass while the retirement went
        // on freezing the wall, which is what actually happened. It is the thing
        // an operator experiences: from the pad being pressed to the new clip
        // being on the card, no single pass costs a frame.
        //
        // The worst pass rather than the mean: a mean over a hundred passes hides
        // exactly the one that dropped six frames.
        const HALF_A_FRAME: Duration = Duration::from_micros(8_333);

        let (mut state, layer_id, catalogue) = show_with_two_clips();
        let mut compositor = HeadlessCompositor::sized(SizePx::new(64, 64));
        let start = Instant::now();
        // `false`: the decoder that ships, and the pool that goes with it.
        let mut playback = Playback::new(start, false);

        // The first clip, all the way onto the card, so what is timed below is a
        // changeover and not a cold start.
        let _ = pixel_at(
            &mut playback,
            &state,
            &catalogue,
            &mut compositor,
            layer_id,
            start,
        );

        // The gesture: the other pad.
        state.layers[0].active_slot = 2;
        state.layers[0].trigger += 1;

        let mut worst = Duration::ZERO;
        let mut arrived = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !arrived && Instant::now() < deadline {
            let began = Instant::now();
            playback.update(&state, &catalogue, &mut compositor, began, a_wall());
            worst = worst.max(began.elapsed());

            arrived = playback
                .layers
                .get(&layer_id)
                .is_some_and(|playing| playing.slot == 2 && playing.uploaded.is_some());
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(arrived, "the second clip never reached the card");
        assert!(
            worst < HALF_A_FRAME,
            "a pass took {worst:?}, and it had to open one clip and let go of \
             another; a frame at sixty is 16.7 ms"
        );
    }
}
