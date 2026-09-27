//! The worker that decodes media off the render thread.
//!
//! One thread, one queue, results collected without blocking. That is the whole
//! design, and the reason for it is the frame budget: at 60 Hz a frame is 16.7
//! ms and decoding a 4K still is more than that, so a decode on the render path
//! would drop frames every time an operator selected a new clip.
//!
//! A request that has not finished yet is not a failure. The layer draws its
//! placeholder for those frames and the image appears when it is ready, which is
//! how a media server behaves when an operator loads something during a show.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use ovs_types::id::MediaItemId;

use crate::media::still::{decode_still, DecodedImage, MediaError};

/// One finished decode.
#[derive(Debug)]
pub struct Loaded {
    /// Which item was being decoded.
    pub media_item_id: MediaItemId,
    /// Where it was read from.
    pub path: PathBuf,
    /// The pixels, or why there are none.
    pub outcome: Result<DecodedImage, MediaError>,
}

/// A request to decode one file.
struct Request {
    media_item_id: MediaItemId,
    path: PathBuf,
}

/// Decodes media on a worker thread.
///
/// Dropping it closes the queue and waits for the worker, so a renderer shutting
/// down does not leave a decode running against a directory that is about to go
/// away.
#[derive(Debug)]
pub struct MediaLoader {
    requests: Option<Sender<Request>>,
    results: Receiver<Loaded>,
    worker: Option<JoinHandle<()>>,
}

impl MediaLoader {
    /// Starts the worker.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the thread cannot be
    /// spawned.
    pub fn spawn() -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel::<Loaded>();

        let worker = std::thread::Builder::new()
            .name("ovs-media-loader".to_owned())
            .spawn(move || {
                // Ends when the request channel closes, which is what dropping
                // the loader does.
                for request in request_rx {
                    let outcome = decode_still(&request.path);
                    if let Err(error) = &outcome {
                        tracing::warn!("{error}");
                    }
                    let loaded = Loaded {
                        media_item_id: request.media_item_id,
                        path: request.path,
                        outcome,
                    };
                    // A closed result channel means the renderer is gone. There
                    // is nothing useful left to decode for.
                    if result_tx.send(loaded).is_err() {
                        return;
                    }
                }
            })?;

        Ok(Self {
            requests: Some(request_tx),
            results: result_rx,
            worker: Some(worker),
        })
    }

    /// Queues one file for decoding.
    ///
    /// Returns false when the worker is gone, which the caller treats as "this
    /// will never arrive" rather than as a reason to stop rendering.
    pub fn request(&self, media_item_id: MediaItemId, path: PathBuf) -> bool {
        self.requests.as_ref().is_some_and(|requests| {
            requests
                .send(Request {
                    media_item_id,
                    path,
                })
                .is_ok()
        })
    }

    /// Takes whatever has finished, without waiting.
    ///
    /// Called once per pass from the render loop, so it must never block: a
    /// decode that is still running simply is not in the result.
    pub fn collect(&self, into: &mut Vec<Loaded>) {
        loop {
            match self.results.try_recv() {
                Ok(loaded) => into.push(loaded),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Waits for the worker to finish what it is doing and stops it.
    ///
    /// Only used by tests, which need a decode to have happened before they can
    /// assert anything about it. The render loop never waits.
    #[cfg(test)]
    fn join(&mut self) {
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for MediaLoader {
    fn drop(&mut self) {
        // Closing the queue is what tells the worker to finish. Joining
        // afterwards means a decode in flight completes against a file that is
        // still there, rather than being torn down mid-read.
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Writes a one-colour PNG and returns its path.
    fn png(directory: &std::path::Path, name: &str, colour: [u8; 4]) -> PathBuf {
        let path = directory.join(name);
        image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
            .save(&path)
            .expect("writes a png");
        path
    }

    /// Collects results until `wanted` of them have arrived, or the test fails.
    fn collect_at_least(loader: &MediaLoader, wanted: usize) -> Vec<Loaded> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut results = Vec::new();
        while results.len() < wanted {
            loader.collect(&mut results);
            assert!(
                Instant::now() < deadline,
                "only {} of {wanted} decodes arrived",
                results.len()
            );
            std::thread::yield_now();
        }
        results
    }

    #[test]
    fn a_requested_image_comes_back_decoded() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [10, 20, 30, 255]);
        let clip = MediaItemId::new();

        let loader = MediaLoader::spawn().expect("starts");
        assert!(loader.request(clip, path.clone()));

        let results = collect_at_least(&loader, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].media_item_id, clip);
        assert_eq!(results[0].path, path);
        let image = results[0].outcome.as_ref().expect("decodes");
        assert_eq!(&image.pixels()[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn collecting_when_nothing_has_finished_returns_immediately() {
        // The property the render path depends on. If this ever blocked, the
        // first frame after a slot change would be late.
        let loader = MediaLoader::spawn().expect("starts");
        let mut results = Vec::new();

        let began = Instant::now();
        loader.collect(&mut results);
        let took = Instant::now().saturating_duration_since(began);

        assert!(results.is_empty());
        assert!(took < Duration::from_millis(50), "collect took {took:?}");
    }

    #[test]
    fn a_failed_decode_comes_back_as_a_failure_not_as_silence() {
        // Silence would be indistinguishable from "still decoding", and the
        // renderer would ask for it again every frame forever.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("broken.png");
        std::fs::write(&path, b"not a png").expect("writes");
        let clip = MediaItemId::new();

        let loader = MediaLoader::spawn().expect("starts");
        assert!(loader.request(clip, path));

        let results = collect_at_least(&loader, 1);
        assert_eq!(results[0].media_item_id, clip);
        assert!(results[0].outcome.is_err());
    }

    #[test]
    fn requests_are_answered_in_the_order_they_were_made() {
        // One worker, one queue. An operator selecting three slots in a row
        // should see them appear in that order rather than in decode-time
        // order, which would depend on file size.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let loader = MediaLoader::spawn().expect("starts");

        let mut expected = Vec::new();
        for index in 0..5_u8 {
            let path = png(
                directory.path(),
                &format!("{index}.png"),
                [index, 0, 0, 255],
            );
            let id = MediaItemId::new();
            expected.push(id);
            assert!(loader.request(id, path));
        }

        let results = collect_at_least(&loader, 5);
        let order: Vec<MediaItemId> = results.iter().map(|loaded| loaded.media_item_id).collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn a_loader_that_is_dropped_stops_its_worker() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [1, 2, 3, 255]);

        let mut loader = MediaLoader::spawn().expect("starts");
        assert!(loader.request(MediaItemId::new(), path));
        loader.join();

        // The worker is gone, so nothing more can be queued. A renderer that
        // kept requesting into a dead loader would wait for images that were
        // never coming.
        assert!(!loader.request(MediaItemId::new(), PathBuf::from("later.png")));
    }
}
