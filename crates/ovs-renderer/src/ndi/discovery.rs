//! What is on the network, for as long as the renderer is running.
//!
//! The operator's list. Somebody choosing a source for a layer picks from what
//! is actually there, so this has to stay current rather than be sampled once:
//! senders are switched on and off all through a rig check, and a list from
//! three hours ago offers sources that have gone and hides the one that just
//! appeared.
//!
//! # Why the renderer and not the core
//!
//! The renderer is the process holding the NDI runtime, exactly as it is the
//! process holding the GPU. [`ovs_ipc::RendererMessage::Connectors`] exists for
//! the same reason and travels the same way: a client cannot offer an operator a
//! list of things to assign without asking the process that can see them.
//!
//! It also keeps the list honest. A source this thread can see is one the
//! receiver beside it can connect to, because it is the same library in the same
//! process on the same network. A list gathered anywhere else could offer
//! sources that would not play.
//!
//! # One finder, not one per look
//!
//! Discovery is mDNS: a finder joins a multicast group and accumulates what it
//! hears. Creating one per question would mean re-joining, waiting out the first
//! answers and throwing away what it learned, several seconds at a time. So the
//! finder is made once and lives as long as the thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use arc_swap::ArcSwap;

use super::library::Ndi;

/// How long each turn round the loop waits for the network to say something.
///
/// The list is read and compared on every turn regardless of what the wait
/// reports, because reading it costs a pointer and a few short strings, and
/// depending on the SDK's idea of "the set changed" would put the accuracy of
/// the operator's list at the mercy of a detail nothing here can check.
const LOOK_MS: u32 = 1_000;

/// The sources on the network, kept current.
pub struct Discovery {
    found: Arc<ArcSwap<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Discovery")
            .field("found", &self.found.load().len())
            .finish()
    }
}

impl Discovery {
    /// Starts looking, and calls `changed` whenever the set of sources differs.
    ///
    /// Called with the whole list rather than what moved, because that is what
    /// goes on the wire and what the operator sees. Called on the discovery
    /// thread, so whatever it does must be quick — storing a vector, which is
    /// what it does.
    ///
    /// Not called for an unchanged list, which is nearly every turn: a quiet
    /// network costs one comparison a second and nothing else.
    #[must_use]
    pub fn start(ndi: &'static Ndi, changed: impl Fn(Vec<String>) + Send + 'static) -> Self {
        let found = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let found = Arc::clone(&found);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("ndi-discovery".to_owned())
                .spawn(move || look(ndi, &found, &stop, changed))
                .ok()
        };
        if thread.is_none() {
            tracing::warn!("could not start a thread to look for NDI sources");
        }
        Self {
            found,
            stop,
            thread,
        }
    }

    /// The sources seen most recently.
    #[must_use]
    pub fn found(&self) -> Arc<Vec<String>> {
        self.found.load_full()
    }
}

impl Drop for Discovery {
    /// Stops looking and waits, so the finder is destroyed before the runtime
    /// is left holding a multicast group nobody is listening on.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!("the NDI discovery thread panicked");
            }
        }
    }
}

/// The discovery thread.
fn look(
    ndi: &'static Ndi,
    found: &Arc<ArcSwap<Vec<String>>>,
    stop: &Arc<AtomicBool>,
    changed: impl Fn(Vec<String>),
) {
    let Some(finder) = ndi.finder() else {
        tracing::warn!("the NDI runtime would not make a finder; no sources will be offered");
        return;
    };

    let mut last: Vec<String> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let now = finder.look(LOOK_MS);
        if now == last {
            continue;
        }

        // Said every time it moves, because this is the line somebody reads
        // when the source they expected is not in the operator's list. Names
        // rather than a count: a count tells them the network is not empty,
        // which they already knew.
        tracing::info!(
            sources = now.len(),
            names = %if now.is_empty() { "none".to_owned() } else { now.join(", ") },
            "NDI sources on this network"
        );
        found.store(Arc::new(now.clone()));
        changed(now.clone());
        last = now;
    }
}
