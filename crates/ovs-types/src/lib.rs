//! Shared domain model for OpenVideoServer.
//!
//! This crate is the single definition of every contract that crosses a process
//! or a language boundary: the show schema, the media model, live state and
//! telemetry. The server, the renderer and the generated TypeScript for the
//! desktop client all come from here, so there is no second copy of the show
//! model to drift out of step.
//!
//! # Structure
//!
//! - [`config`] — persistent configuration. Written to disk, survives restarts.
//! - [`live`] — live state. Driven by the operator, the clock or a lighting
//!   console. Never persisted as show data.
//! - [`media`] — logical media items and their physical assets.
//! - [`telemetry`] — measured server and output state.
//! - [`version`] — versions of every contract that can outlive a build.
//!
//! The split between [`config`] and [`live`] is the one that matters most. It
//! exists so that a transient Art-Net value cannot be written into a saved
//! show: the persistence layer accepts only [`config`] types,
//! which makes the mistake a compile error rather than a code review item.
//!
//! # What this crate does not contain
//!
//! No I/O, no GPU, no async runtime, no allocation-heavy helpers. It is linked
//! into the realtime renderer, so it stays small and predictable.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod account;
pub mod config;
pub mod depth;
pub mod error;
pub mod geometry;
pub mod id;
pub mod live;
pub mod media;
pub mod schema;
pub mod telemetry;
pub mod time;
pub mod version;

pub use error::{SchemaError, ValidationError, ValidationErrors};
pub use version::{API_VERSION, REALTIME_PROTOCOL_VERSION, SHOW_SCHEMA_VERSION};

#[cfg(test)]
mod invariants {
    //! Tests for the architectural invariants.
    //!
    //! These are not unit tests of one function. Each one pins down a rule that
    //! the specification calls non-optional, so that a later refactor that
    //! quietly breaks the rule fails here with the rule's name attached.

    use crate::config::{Layer, Route, Show, Slot};
    use crate::id::MediaItemId;

    #[test]
    fn invariant_5_slot_zero_is_always_black() {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(0, MediaItemId::new()));
        show.layers.push(layer);

        show.validate()
            .expect_err("slot 0 must never be configurable");
    }

    #[test]
    fn invariant_6_every_layer_routes_to_exactly_one_bus() {
        // Route is a single enum value, so a layer cannot feed two buses or none.
        let layer = Layer::new("Layer 1", 0);
        assert!(matches!(layer.route, Route::A | Route::B | Route::M));
    }

    #[test]
    fn invariant_8_media_identity_is_independent_of_filenames() {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(layer);

        let json = serde_json::to_string(&show).expect("serialises");
        for forbidden in ["object_key", "sha256", "original_filename", "/storage/"] {
            assert!(
                !json.contains(forbidden),
                "a show must not contain {forbidden}: physical identity belongs to MediaAsset"
            );
        }
    }

    #[test]
    fn invariant_12_there_is_no_artificial_layer_limit() {
        let mut show = Show::new("Show");
        for order in 0..256 {
            show.layers
                .push(Layer::new(format!("Layer {order}"), order));
        }
        assert_eq!(
            show.validate(),
            Ok(()),
            "layer count must be limited by hardware, not by rule"
        );
    }

    #[test]
    fn invariant_13_every_new_show_records_its_schema_version() {
        // That the version constants themselves are sane is asserted at compile
        // time in `version.rs`; this checks that a show actually carries one.
        assert_eq!(Show::new("Show").schema_version, crate::SHOW_SCHEMA_VERSION);
        let json = serde_json::to_string(&Show::new("Show")).expect("serialises");
        assert!(json.contains("\"schema_version\""));
    }

    #[test]
    fn invariant_4_a_layer_owns_nothing_about_time() {
        // **Nothing at all**, as of 2026-09-23. It used to own a speed ratio,
        // then only a trigger grid, and now neither: "Layer haben keinen Takt
        // oder Beat, nur Slots haben das."
        //
        // A slot says how long its own footage takes and how fast to run
        // through it; the master says what a beat is. A layer is a track, and a track
        // has no tempo of its own — any field here would be a second answer to
        // a question that has one.
        //
        // Serialising one is the check, because what a layer *writes* is what a
        // show carries: a field kept only to be read out of an older
        // document must not come back in a new one.
        let json = serde_json::to_string(&Layer::new("Layer 1", 0)).expect("serialises");
        assert!(
            !json.contains("quantization"),
            "a layer must never write a grid"
        );
        assert!(!json.contains("bpm"), "a layer must never own a BPM");
        assert!(
            !json.contains("speed"),
            "a layer must never own a rate either"
        );

        // The trigger grid does not exist anywhere any more (owner,
        // 2026-09-23: "Trigger Grid weg. Beats ist das was man braucht."), so
        // a slot must not have grown one either.
        let mut slot = Slot::media(1, MediaItemId::new());
        slot.sync = crate::config::SlotSync::BeatSync { beats: 7 };
        slot.speed = 2.0;
        let json = serde_json::to_string(&slot).expect("serialises");
        assert!(
            !json.contains("quantization"),
            "nothing in the model has a trigger grid"
        );
        // What a slot does own about time: how many beats a pass takes, and how
        // fast it runs through one.
        assert!(json.contains("beats"), "a slot carries its own pass length");
        assert!(json.contains("speed"), "a slot carries its own rate");
    }
}
