//! The media path end to end, over a real socket and with no GPU.
//!
//! Each half of this is tested on its own: the core resolves keys to paths, the
//! channel carries a catalogue, the renderer decodes off the render thread and
//! keeps textures resident. What no unit test can show is that the two ends
//! agree — that the path the core sends is one the renderer can actually open,
//! and that a media item registered in a database ends up as pixels on an
//! output without anything in between having to be told twice.
//!
//! Headless throughout, so it runs on a CI machine with no GPU. What it proves
//! is the plumbing; that the pixels are the right way up on a real device is
//! `a_decoded_png_reaches_the_canvas_the_right_way_up`, which needs one.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ovs_core::{renderer_link, AppState, MediaStore};
use ovs_db::Database;
use ovs_renderer::backend::{Compositor, HeadlessBackend, HeadlessCompositor, OutputBackend};
use ovs_renderer::control;
use ovs_renderer::render_loop::RenderLoop;
use ovs_renderer::show_state::SharedShowState;
use ovs_types::config::{DisplayMode, Layer, Show, Slot};
use ovs_types::geometry::{FrameRate, SizePx};
use ovs_types::id::{MediaAssetId, MediaItemId};
use ovs_types::media::{AssetValidation, MediaAsset, MediaItem, MediaKind, MediaState};
use ovs_types::time::Timestamp;

/// Writes a one-colour PNG into the object store and registers it.
///
/// Returns the item's identifier. The hash is made up rather than computed:
/// verification is the ingest pipeline's job, and this test is about what
/// happens after an asset is already registered.
fn register_still(state: &AppState, store: &MediaStore, sha256: &str) -> MediaItemId {
    let item = MediaItem {
        kind: MediaKind::Image,
        state: MediaState::Ready,
        duration_ms: None,
        frame_rate: None,
        dimensions: Some(SizePx::new(2, 2)),
        ..MediaItem::importing("Backdrop", "backdrop.png")
    };
    let object_key = MediaAsset::object_key_for(sha256, "png").expect("a valid key");
    let asset = MediaAsset {
        id: MediaAssetId::new(),
        media_item_id: item.id,
        object_key: object_key.clone(),
        sha256: sha256.to_owned(),
        byte_size: 0,
        container: "png".to_owned(),
        codec: "png".to_owned(),
        validation: AssetValidation::Verified {
            at: Timestamp::now(),
        },
        created_at: Timestamp::now(),
    };

    state.with_database(|database| {
        database.register_media(&item, &asset).expect("registers");
    });

    let path = store.path_for(&object_key).expect("resolves");
    write_png(&path, [10, 20, 30, 255]);
    item.id
}

/// Writes a two-by-two PNG, creating the directories it needs.
fn write_png(path: &Path, colour: [u8; 4]) {
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates");
    image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
        .save_with_format(path, image::ImageFormat::Png)
        .expect("writes a png");
}

/// A show whose one layer holds `clip` in slot 1.
///
/// Returns the show and the layer, because a freshly loaded layer sits on
/// slot 0 — the permanently black one — until something selects another. That
/// is the restart policy doing what it should, so the test selects a
/// slot the way an operator would rather than assuming media is on screen.
fn show_using(clip: MediaItemId) -> (Show, ovs_types::id::LayerId) {
    let mut show = Show::new("Show");
    let mut layer = Layer::new("Layer 1", 0);
    layer.slots.push(Slot::media(1, clip));
    let layer_id = layer.id;
    show.layers.push(layer);
    (show, layer_id)
}

/// Loads the show and puts its layer on the slot holding the media.
fn load_and_select(state: &AppState, clip: MediaItemId) {
    let (show, layer_id) = show_using(clip);
    state.mutate_state(|current| {
        let change = current.load_show(show, None);
        ((), Some(change))
    });
    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 1).expect("a real slot");
        ((), change)
    });
}

/// Everything running, in the order a real server starts it.
struct Running {
    running: Arc<AtomicBool>,
    address: String,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Running {
    fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        // The renderer's accept loop only looks at the flag between
        // connections, so one more connection is what lets it see it. A real
        // renderer exits the process instead; a test has to be tidier than that.
        let _ = std::net::TcpStream::connect(&self.address);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[test]
fn a_registered_still_becomes_a_texture_on_an_output() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let media_root = directory.path().join("media");
    let store = MediaStore::new(&media_root);
    store.ensure_root().expect("creates the store");

    let database = Database::open(directory.path().join("ovs.db")).expect("opens");
    let state = AppState::with_media_root(database, "0.0.1-test", &media_root);
    let clip = register_still(&state, &store, &"a".repeat(64));

    // The renderer listens and the core connects to it, which is the direction
    // that lets the core restart without the renderer noticing.
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let address = listener.local_addr().expect("has an address").to_string();

    let shared = Arc::new(SharedShowState::new());
    let running = Arc::new(AtomicBool::new(true));

    let control_state = Arc::clone(&shared);
    let control_running = Arc::clone(&running);
    let control_thread = std::thread::Builder::new()
        .name("test-control".to_owned())
        .spawn(move || {
            let _ = control::serve(&listener, &control_state, &control_running);
        })
        .expect("spawns");

    let link_state = state.clone();
    let link_address = address.clone();
    let link_running = Arc::clone(&running);
    let link_thread = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_running))
        .expect("spawns");

    let live = Running {
        running: Arc::clone(&running),
        address: address.clone(),
        threads: vec![control_thread, link_thread],
    };

    // The show is loaded after both ends are up, so the catalogue arrives as an
    // update rather than as part of the first snapshot. That is the harder of
    // the two orderings and the one an operator actually produces.
    load_and_select(&state, clip);

    let epoch = Instant::now();
    let outputs: Vec<Box<dyn OutputBackend>> = vec![Box::new(HeadlessBackend::new(
        "Wall",
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
    ))];
    let mut render = RenderLoop::new(
        Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
        outputs,
        Arc::clone(&shared),
        epoch,
    )
    .expect("valid rates");

    let frame = Duration::from_nanos(
        FrameRate::FPS_60
            .frame_duration_nanos()
            .expect("valid rate"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut index = 0_u32;
    while !render.media_is_resident(clip) {
        render.tick(epoch + frame * index + frame / 2);
        assert!(
            Instant::now() < deadline,
            "the still never reached the output"
        );
        index += 1;
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(render.resident_media(), 1);
    assert_eq!(render.composed_quads(), 1, "and the layer is being drawn");

    live.stop();
}

#[test]
fn a_picture_only_a_live_value_names_reaches_the_renderer() {
    // **The gap the Mask effect found** (on the machine, 2026-10-13). The
    // catalogue the renderer is sent is resolved from what the show document
    // references, and an operator picking a mask in the live panel has saved
    // nothing — so the value arrived, the pass ran, and the renderer had never
    // been told where the file was. What it drew was the white default, which
    // is a mask that does nothing: the layer came up unmasked with no fault
    // anywhere to explain it.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let media_root = directory.path().join("media");
    let store = MediaStore::new(&media_root);
    store.ensure_root().expect("creates the store");

    let database = Database::open(directory.path().join("ovs.db")).expect("opens");
    let state = AppState::with_media_root(database, "0.0.1-test", &media_root);
    let clip = register_still(&state, &store, &"a".repeat(64));
    let stencil = register_still(&state, &store, &"b".repeat(64));

    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let address = listener.local_addr().expect("has an address").to_string();
    let shared = Arc::new(SharedShowState::new());
    let running = Arc::new(AtomicBool::new(true));

    let control_state = Arc::clone(&shared);
    let control_running = Arc::clone(&running);
    let control_thread = std::thread::Builder::new()
        .name("test-control".to_owned())
        .spawn(move || {
            let _ = control::serve(&listener, &control_state, &control_running);
        })
        .expect("spawns");

    let link_state = state.clone();
    let link_address = address.clone();
    let link_running = Arc::clone(&running);
    let link_thread = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_running))
        .expect("spawns");

    let live = Running {
        running: Arc::clone(&running),
        address: address.clone(),
        threads: vec![control_thread, link_thread],
    };

    // A show with a mask on the layer and no picture in it, which is what
    // dragging the effect onto a layer gives you.
    let (mut show, layer_id) = show_using(clip);
    let masking = ovs_types::config::EffectInstance::new("ovs.mask", 1);
    let effect_instance_id = masking.id;
    show.layers[0].effects.push(masking);
    state.mutate_state(|current| {
        let change = current.load_show(show, None);
        ((), Some(change))
    });
    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 1).expect("a real slot");
        ((), change)
    });

    // And the picture chosen live, which is the whole point: nothing is saved.
    let overrides = ovs_types::config::ParameterMap::from([(
        "image".to_owned(),
        ovs_types::config::slot::ParameterValue::Text(stencil.to_string()),
    )]);
    state.mutate_state(|current| {
        let applied = current.set_effect_parameters(effect_instance_id, &overrides, false);
        match applied {
            Ok(change) => ((), change),
            Err(error) => panic!("the live value was refused: {error}"),
        }
    });

    let epoch = Instant::now();
    let outputs: Vec<Box<dyn OutputBackend>> = vec![Box::new(HeadlessBackend::new(
        "Wall",
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
    ))];
    let mut render = RenderLoop::new(
        Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
        outputs,
        Arc::clone(&shared),
        epoch,
    )
    .expect("valid rates");

    let frame = Duration::from_nanos(
        FrameRate::FPS_60
            .frame_duration_nanos()
            .expect("valid rate"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut index = 0_u32;
    while !render.media_is_resident(stencil) {
        render.tick(epoch + frame * index + frame / 2);
        assert!(
            Instant::now() < deadline,
            "the picture a live value named never reached the card"
        );
        index += 1;
        std::thread::sleep(Duration::from_millis(1));
    }

    live.stop();
}

#[test]
fn media_the_core_cannot_resolve_leaves_the_show_running() {
    // An asset registered against a file that is not there is what a
    // half-restored backup looks like. The renderer must keep presenting, and
    // the layer must keep drawing its placeholder, rather than either end
    // treating a missing file as a fault.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let media_root = directory.path().join("media");
    let store = MediaStore::new(&media_root);
    store.ensure_root().expect("creates the store");

    let database = Database::open(directory.path().join("ovs.db")).expect("opens");
    let state = AppState::with_media_root(database, "0.0.1-test", &media_root);
    let clip = register_still(&state, &store, &"b".repeat(64));

    // The file goes missing after registration.
    let object_key = MediaAsset::object_key_for(&"b".repeat(64), "png").expect("a valid key");
    let path: PathBuf = store.path_for(&object_key).expect("resolves");
    std::fs::remove_file(&path).expect("removes");

    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let address = listener.local_addr().expect("has an address").to_string();
    let shared = Arc::new(SharedShowState::new());
    let running = Arc::new(AtomicBool::new(true));

    let control_state = Arc::clone(&shared);
    let control_running = Arc::clone(&running);
    let control_thread = std::thread::Builder::new()
        .name("test-control".to_owned())
        .spawn(move || {
            let _ = control::serve(&listener, &control_state, &control_running);
        })
        .expect("spawns");

    load_and_select(&state, clip);

    let link_state = state.clone();
    let link_address = address.clone();
    let link_running = Arc::clone(&running);
    let link_thread = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_running))
        .expect("spawns");

    let live = Running {
        running: Arc::clone(&running),
        address: address.clone(),
        threads: vec![control_thread, link_thread],
    };

    let epoch = Instant::now();
    let outputs: Vec<Box<dyn OutputBackend>> = vec![Box::new(HeadlessBackend::new(
        "Wall",
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
    ))];
    let mut render = RenderLoop::new(
        Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
        outputs,
        Arc::clone(&shared),
        epoch,
    )
    .expect("valid rates");

    let frame = Duration::from_nanos(
        FrameRate::FPS_60
            .frame_duration_nanos()
            .expect("valid rate"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut index = 0_u32;
    let mut presented = 0;
    while Instant::now() < deadline && index < 120 {
        presented += render.tick(epoch + frame * index + frame / 2).presented;
        index += 1;
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(presented > 0, "the show kept running");
    assert_eq!(render.resident_media(), 0, "there was nothing to load");
    assert_eq!(render.composed_quads(), 1, "the layer still draws flat");

    live.stop();
}
