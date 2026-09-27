//! What happens when something goes away mid-show.
//!
//! The three failures worth naming: a renderer restart and what it
//! comes back to, a client disconnecting without the renderer noticing, and a
//! failing preview leaving the show alone. All of them are about the same
//! property — that the picture on the wall depends on as little as possible —
//! and none of them can be tested on one side alone.
//!
//! Headless throughout, so this runs on a machine with no GPU.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ovs_core::{renderer_link, AppState};
use ovs_db::Database;
use ovs_renderer::backend::{Compositor, HeadlessBackend, HeadlessCompositor, OutputBackend};
use ovs_renderer::control;
use ovs_renderer::render_loop::RenderLoop;
use ovs_renderer::show_state::SharedShowState;
use ovs_types::config::{DisplayMode, Layer, Show, Slot};
use ovs_types::geometry::{FrameRate, SizePx};
use ovs_types::id::{LayerId, MediaItemId};
use ovs_types::telemetry::RendererStatus;

/// A show with one layer holding a media reference in slot 1.
fn show() -> (Show, LayerId) {
    let mut show = Show::new("Show");
    let mut layer = Layer::new("Layer 1", 0);
    layer.slots.push(Slot::media(1, MediaItemId::new()));
    let layer_id = layer.id;
    show.layers.push(layer);
    (show, layer_id)
}

/// A core with a show loaded and a layer on slot 1.
fn core_with_a_show() -> (AppState, LayerId, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let database = Database::open(directory.path().join("ovs.db")).expect("opens");
    let state = AppState::new(database, "0.0.1-test");

    let (show, layer_id) = show();
    state.mutate_state(|current| {
        let change = current.load_show(show, None);
        ((), Some(change))
    });
    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 1).expect("a real slot");
        ((), change)
    });

    (state, layer_id, directory)
}

/// One renderer end: a listener, its control thread and the state it fills.
struct Renderer {
    shared: Arc<SharedShowState>,
    running: Arc<AtomicBool>,
    address: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Renderer {
    /// Starts a renderer listening on an address the operating system picks.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        let address = listener.local_addr().expect("has an address").to_string();

        let shared = Arc::new(SharedShowState::new());
        let running = Arc::new(AtomicBool::new(true));

        let control_state = Arc::clone(&shared);
        let control_running = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name("test-control".to_owned())
            .spawn(move || {
                let _ = control::serve(&listener, &control_state, &control_running);
            })
            .expect("spawns");

        Self {
            shared,
            running,
            address,
            thread: Some(thread),
        }
    }

    /// Starts one on a known address, as a restarted renderer would.
    ///
    /// Retries the bind: the port the last one was listening on may still have
    /// its closed connection in the operating system's books for a moment, and
    /// a supervisor restarting a process hits exactly the same wait.
    fn start_on(address: &str) -> Self {
        let deadline = Instant::now() + Duration::from_secs(10);
        let listener = loop {
            match TcpListener::bind(address) {
                Ok(listener) => break listener,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("could not listen on {address} again: {error}"),
            }
        };
        let shared = Arc::new(SharedShowState::new());
        let running = Arc::new(AtomicBool::new(true));

        let control_state = Arc::clone(&shared);
        let control_running = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name("test-control".to_owned())
            .spawn(move || {
                let _ = control::serve(&listener, &control_state, &control_running);
            })
            .expect("spawns");

        Self {
            shared,
            running,
            address: address.to_owned(),
            thread: Some(thread),
        }
    }

    /// Waits for it to stop on its own.
    ///
    /// What a renderer does when the core asks it to: the control loop returns
    /// and the process ends. Nothing here forces it, because forcing it would
    /// be testing something no operator can cause.
    fn wait_for_shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Stops it, the way a renderer process ending would.
    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // The accept loop only looks at the flag between connections, so one
        // more connection is what lets it see it.
        let _ = std::net::TcpStream::connect(&self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs `check` until it passes or the deadline goes by.
fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} never happened");
}

/// A headless render loop driving one output.
fn render_loop(shared: &Arc<SharedShowState>) -> (RenderLoop, Instant) {
    let epoch = Instant::now();
    let outputs: Vec<Box<dyn OutputBackend>> = vec![Box::new(HeadlessBackend::new(
        "Wall",
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
    ))];
    let render = RenderLoop::new(
        Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
        outputs,
        Arc::clone(shared),
        epoch,
    )
    .expect("valid rates");
    (render, epoch)
}

#[test]
fn a_restarted_renderer_comes_back_to_the_show_that_is_running() {
    // Renderer restart → recovery. The core does not stop when
    // the renderer does, so what the renderer must come back to is not what it
    // was sent before it went away — it is whatever the show is now.
    let (state, layer_id, _directory) = core_with_a_show();

    let mut first = Renderer::start();
    let address = first.address.clone();

    let link_running = Arc::new(AtomicBool::new(true));
    let link_state = state.clone();
    let link_address = address.clone();
    let link_flag = Arc::clone(&link_running);
    let link = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_flag))
        .expect("spawns");

    wait_until("the first renderer received the show", || {
        first.shared.load().has_show()
    });
    wait_until("the core noticed the renderer", || {
        state.with_state(|current| matches!(current.renderer(), RendererStatus::Running))
    });

    // The renderer is restarted the way an operator restarts it: the core asks
    // it to stop and a supervisor starts it again. Everything else carries on,
    // and the show moves on while nothing is rendering — which is what makes
    // this a recovery test rather than a reconnect test.
    state.request_renderer_restart();
    first.wait_for_shutdown();
    wait_until("the core noticed it had gone", || {
        state.with_state(|current| !matches!(current.renderer(), RendererStatus::Running))
    });

    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 0).expect("the black slot");
        ((), change)
    });

    // And a new one starts on the same address, as a supervisor would start it.
    let mut second = Renderer::start_on(&address);
    wait_until("the new renderer received the show", || {
        second.shared.load().has_show()
    });

    let seen = second.shared.load();
    let live = seen.layer(layer_id).expect("the layer");
    assert_eq!(
        live.active_slot, 0,
        "the renderer came back to the show as it is now, not as it was"
    );

    wait_until("the core saw the new renderer", || {
        state.with_state(|current| matches!(current.renderer(), RendererStatus::Running))
    });

    link_running.store(false, Ordering::Relaxed);
    second.stop();
    let _ = link.join();
}

#[test]
fn a_client_going_away_does_not_touch_the_renderer() {
    // Client disconnect → unchanged renderer. A client is a
    // subscriber, not a participant: a laptop that closes its lid must not be
    // able to change what is on a wall.
    let (state, layer_id, _directory) = core_with_a_show();

    let mut renderer = Renderer::start();
    let link_running = Arc::new(AtomicBool::new(true));
    let link_state = state.clone();
    let link_address = renderer.address.clone();
    let link_flag = Arc::clone(&link_running);
    let link = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_flag))
        .expect("spawns");

    wait_until("the renderer received the show", || {
        renderer.shared.load().has_show()
    });

    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 1).expect("a real slot");
        ((), change)
    });
    wait_until("the renderer followed the slot", || {
        renderer
            .shared
            .load()
            .layer(layer_id)
            .is_some_and(|live| live.active_slot == 1)
    });

    // A client subscribes and then vanishes without a word, which is what
    // pulling a network cable looks like from here.
    let subscriber = state.subscribe();
    let sequence_before = state.with_state(|current| current.sequence());
    drop(subscriber);

    // The show carries on, and the renderer keeps following it.
    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 0).expect("the black slot");
        ((), change)
    });
    wait_until("the renderer followed the next change too", || {
        renderer
            .shared
            .load()
            .layer(layer_id)
            .is_some_and(|live| live.active_slot == 0)
    });

    let sequence_after = state.with_state(|current| current.sequence());
    assert!(
        sequence_after > sequence_before,
        "the show moved on after the client left"
    );
    assert!(
        renderer.shared.is_connected(),
        "and the renderer never noticed"
    );

    link_running.store(false, Ordering::Relaxed);
    renderer.stop();
    let _ = link.join();
}

#[test]
fn an_output_that_cannot_produce_a_preview_keeps_presenting() {
    // Preview failure → unchanged renderer, and the rule that
    // a preview must never be able to stall the render path. A preview is a tap
    // for somebody watching; the wall is the show.
    let shared = Arc::new(SharedShowState::new());
    let (mut render, epoch) = render_loop(&shared);

    let frame = Duration::from_nanos(
        FrameRate::FPS_60
            .frame_duration_nanos()
            .expect("valid rate"),
    );

    // The headless output offers no picture at all, which is the same shape of
    // answer as a readback that failed.
    let previews = render.take_previews();
    assert!(previews.is_empty(), "there is no picture to take");

    let mut presented = 0;
    for index in 1..=10 {
        let report = render.tick(epoch + frame * index + frame / 2);
        presented += report.presented;
        let _ = render.take_previews();
    }

    assert!(
        presented >= 5,
        "the loop kept presenting while previews produced nothing: {presented}"
    );
    assert!(render.is_rendering());
}
