//! Which media is on the GPU, and which is on its way there.
//!
//! The render loop asks for a still by naming it; this decides whether that
//! means "already there", "already asked for", "asked for and it failed" or
//! "ask now". Getting those four apart is the whole job: conflating the last two
//! makes a broken file into sixty decode attempts a second, and conflating the
//! first two makes an image that is on its way be asked for again every frame.
//!
//! Nothing here opens a file. It queues work for [`MediaLoader`] and hands
//! finished pixels to the outputs, both of which are cheap.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use ovs_types::id::MediaItemId;

use crate::backend::Compositor;
use crate::media::{Loaded, MediaLoader};
use crate::show_state::MediaCatalogue;

/// How many media items may hold textures at once.
///
/// Below what the compositor's descriptor pool holds, so reaching this limit is
/// a decision made here rather than a refusal from the driver. A show switching
/// between more than this many stills pays a decode when it comes back to one it
/// has not used for a while, which is the right trade against unbounded GPU
/// memory on a machine that also has to keep presenting.
pub const MAX_RESIDENT: usize = 48;

/// One item whose pixels are on the GPU.
#[derive(Debug)]
struct Resident {
    /// The file the texture was decoded from.
    ///
    /// Compared against the catalogue, so re-transcoded media is noticed:
    /// a new asset is a new path, and the pixels on the GPU are the old ones.
    path: PathBuf,
    /// When a scene last asked for it, for eviction order.
    last_wanted: Instant,
}

/// What one pass of media work did, for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaReport {
    /// Files queued for decoding this pass.
    pub requested: usize,
    /// Images uploaded to the outputs this pass.
    pub uploaded: usize,
    /// Textures freed this pass.
    pub evicted: usize,
    /// Decodes that came back as failures this pass.
    pub failed: usize,
}

/// Tracks what is resident, pending and known-broken.
#[derive(Debug)]
pub struct Residency {
    resident: BTreeMap<MediaItemId, Resident>,
    /// Queued for decoding, with the file that was queued.
    pending: BTreeMap<MediaItemId, PathBuf>,
    /// Tried and failed, with the file that failed.
    ///
    /// An entry here is not retried. The catalogue naming a different file
    /// clears it, which is what happens when the media is re-imported — so a
    /// broken image is attempted once, and fixing it does not need a restart.
    failed: BTreeMap<MediaItemId, PathBuf>,
    /// Reused between passes, so the render path allocates nothing.
    results: Vec<Loaded>,
}

impl Default for Residency {
    fn default() -> Self {
        Self::new()
    }
}

impl Residency {
    /// Creates an empty residency.
    #[must_use]
    pub fn new() -> Self {
        Self {
            resident: BTreeMap::new(),
            pending: BTreeMap::new(),
            failed: BTreeMap::new(),
            results: Vec::new(),
        }
    }

    /// Returns how many items currently have textures.
    #[must_use]
    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }

    /// Returns true when this item's pixels are on the GPU.
    #[must_use]
    pub fn is_resident(&self, media_item_id: MediaItemId) -> bool {
        self.resident.contains_key(&media_item_id)
    }

    /// Forgets everything, without freeing anything.
    ///
    /// For when the GPU-side textures have gone by other means — the compositor
    /// they lived on was replaced — and the bookkeeping would otherwise claim
    /// images that no longer exist are still resident.
    pub fn forget_everything(&mut self) {
        self.resident.clear();
        self.failed.clear();
    }

    /// Returns true when this item was tried and could not be decoded.
    #[must_use]
    pub fn has_failed(&self, media_item_id: MediaItemId) -> bool {
        self.failed.contains_key(&media_item_id)
    }

    /// Does one pass of media work: collect, upload, request, evict.
    ///
    /// `wanted` is what the scene about to be drawn refers to, in no particular
    /// order. It comes from the show state rather than from the composed scene,
    /// so a layer faded to nothing keeps its texture instead of losing it and
    /// having to decode again the moment it is faded up.
    pub fn update(
        &mut self,
        loader: &MediaLoader,
        catalogue: &MediaCatalogue,
        wanted: &[MediaItemId],
        compositor: &mut dyn Compositor,
        now: Instant,
    ) -> MediaReport {
        let mut report = MediaReport::default();

        self.take_finished(loader, compositor, &mut report);
        self.forget_what_the_catalogue_dropped(compositor, catalogue, &mut report);
        self.request_missing(loader, catalogue, wanted, now, &mut report);
        self.evict_down_to_the_limit(compositor, wanted, &mut report);

        report
    }

    /// Uploads whatever the loader finished, and records what failed.
    fn take_finished(
        &mut self,
        loader: &MediaLoader,
        compositor: &mut dyn Compositor,
        report: &mut MediaReport,
    ) {
        self.results.clear();
        loader.collect(&mut self.results);

        for loaded in self.results.drain(..) {
            // A result for something no longer pending is one the catalogue
            // changed under: the pixels are for a file nobody is asking for any
            // more, so they are dropped rather than uploaded.
            let Some(queued) = self.pending.remove(&loaded.media_item_id) else {
                continue;
            };
            if queued != loaded.path {
                continue;
            }

            match loaded.outcome {
                Ok(image) => match compositor.upload_texture(loaded.media_item_id.into(), &image) {
                    Ok(()) => {
                        // Once per load, never per frame. An operator watching a
                        // log wants to know their clip arrived, and a renderer
                        // that says nothing at all leaves "is it loading or is
                        // it broken" as a question for showtime.
                        tracing::info!(
                            media_item_id = %loaded.media_item_id,
                            width = image.size().width,
                            height = image.size().height,
                            "media loaded"
                        );
                        self.resident.insert(
                            loaded.media_item_id,
                            Resident {
                                path: loaded.path,
                                last_wanted: Instant::now(),
                            },
                        );
                        report.uploaded += 1;
                    }
                    Err(error) => {
                        // A refusal now will be a refusal next frame too, so it
                        // is recorded against this file rather than retried
                        // sixty times a second. A re-import clears it.
                        tracing::warn!("{error}");
                        self.failed.insert(loaded.media_item_id, loaded.path);
                        report.failed += 1;
                    }
                },
                Err(error) => {
                    // Already logged by the worker; recorded here so it is not
                    // attempted again every frame for the rest of the show.
                    let _ = error;
                    self.failed.insert(loaded.media_item_id, loaded.path);
                    report.failed += 1;
                }
            }
        }
    }

    /// Frees textures for media the catalogue no longer offers, or offers from a
    /// different file.
    fn forget_what_the_catalogue_dropped(
        &mut self,
        compositor: &mut dyn Compositor,
        catalogue: &MediaCatalogue,
        report: &mut MediaReport,
    ) {
        let stale: Vec<MediaItemId> = self
            .resident
            .iter()
            .filter(|(id, resident)| {
                catalogue
                    .get(*id)
                    .is_none_or(|entry| entry.path != resident.path)
            })
            .map(|(id, _)| *id)
            .collect();

        for id in stale {
            self.free(compositor, id);
            report.evicted += 1;
        }

        // A failure recorded against a file the catalogue no longer names is not
        // a failure any more. This is how re-importing broken media fixes it
        // without restarting the renderer.
        self.failed.retain(|id, path| {
            catalogue
                .get(id)
                .is_some_and(|entry| entry.path.as_path() == path.as_path())
        });
    }

    /// Queues anything wanted that is neither resident, pending nor known bad.
    fn request_missing(
        &mut self,
        loader: &MediaLoader,
        catalogue: &MediaCatalogue,
        wanted: &[MediaItemId],
        now: Instant,
        report: &mut MediaReport,
    ) {
        for &id in wanted {
            if let Some(resident) = self.resident.get_mut(&id) {
                resident.last_wanted = now;
                continue;
            }
            if self.pending.contains_key(&id) || self.failed.contains_key(&id) {
                continue;
            }
            // Not in the catalogue means the core has not said where it is, or
            // there is nothing playable. The layer draws its placeholder.
            let Some(entry) = catalogue.get(&id) else {
                continue;
            };

            if loader.request(id, entry.path.clone()) {
                self.pending.insert(id, entry.path.clone());
                report.requested += 1;
            }
        }
    }

    /// Frees the least recently wanted textures until the limit is met.
    fn evict_down_to_the_limit(
        &mut self,
        compositor: &mut dyn Compositor,
        wanted: &[MediaItemId],
        report: &mut MediaReport,
    ) {
        while self.resident.len() > MAX_RESIDENT {
            // Never something the scene is asking for right now: evicting that
            // would free a texture and immediately re-request it, which is a
            // decode per frame for as long as the show stayed over the limit.
            let victim = self
                .resident
                .iter()
                .filter(|(id, _)| !wanted.contains(id))
                .min_by_key(|(_, resident)| resident.last_wanted)
                .map(|(id, _)| *id);

            let Some(victim) = victim else {
                // Everything resident is on screen. Over the limit is better
                // than a frame with holes in it.
                return;
            };
            self.free(compositor, victim);
            report.evicted += 1;
        }
    }

    /// Drops one item's texture.
    fn free(&mut self, compositor: &mut dyn Compositor, media_item_id: MediaItemId) {
        compositor.drop_texture(media_item_id.into());
        self.resident.remove(&media_item_id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ovs_ipc::MediaLocation;
    use ovs_types::geometry::SizePx;
    use ovs_types::media::MediaKind;

    use crate::backend::HeadlessCompositor;

    use super::*;

    /// Writes a one-colour PNG and returns its path.
    fn png(directory: &std::path::Path, name: &str, colour: [u8; 4]) -> PathBuf {
        let path = directory.join(name);
        image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
            .save(&path)
            .expect("writes a png");
        path
    }

    fn located(media_item_id: MediaItemId, path: &std::path::Path) -> MediaLocation {
        MediaLocation {
            media_item_id,
            kind: MediaKind::Image,
            path: path.to_path_buf(),
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }
    }

    fn catalogue(entries: Vec<MediaLocation>) -> MediaCatalogue {
        entries
            .into_iter()
            .map(|entry| (entry.media_item_id, entry))
            .collect()
    }

    /// Runs passes until `done` or the test gives up.
    ///
    /// A decode happens on another thread, so a test has to let time pass. The
    /// deadline is what turns "this will never happen" into a failure rather
    /// than a hang.
    fn run_until(
        residency: &mut Residency,
        loader: &MediaLoader,
        catalogue: &MediaCatalogue,
        wanted: &[MediaItemId],
        compositor: &mut dyn Compositor,
        done: impl Fn(&Residency) -> bool,
    ) -> MediaReport {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut total = MediaReport::default();
        loop {
            let report = residency.update(loader, catalogue, wanted, compositor, Instant::now());
            total.requested += report.requested;
            total.uploaded += report.uploaded;
            total.evicted += report.evicted;
            total.failed += report.failed;

            if done(residency) {
                return total;
            }
            assert!(Instant::now() < deadline, "gave up waiting: {total:?}");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_wanted_image_is_decoded_once_and_uploaded_once() {
        // Once, and to the compositor rather than to each output: the
        // composition pass is the only pass that samples media, so a copy per
        // wall would be the same pixels three times on one card.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [10, 20, 30, 255]);
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();
        let catalogue = catalogue(vec![located(clip, &path)]);

        let total = run_until(
            &mut residency,
            &loader,
            &catalogue,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );

        assert_eq!(total.requested, 1, "asked for exactly once");
        assert_eq!(total.uploaded, 1);
        assert_eq!(
            gpu.texture(clip),
            Some((SizePx::new(2, 2), [10, 20, 30, 255]))
        );
    }

    #[test]
    fn an_image_already_resident_is_never_asked_for_again() {
        // Sixty passes a second for the length of a show. Asking again each time
        // would be a decode per frame of something already on the GPU.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [1, 2, 3, 255]);
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();
        let catalogue = catalogue(vec![located(clip, &path)]);

        run_until(
            &mut residency,
            &loader,
            &catalogue,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );

        let mut later = MediaReport::default();
        for _ in 0..60 {
            let report = residency.update(&loader, &catalogue, &[clip], &mut gpu, Instant::now());
            later.requested += report.requested;
            later.uploaded += report.uploaded;
        }
        assert_eq!(later.requested, 0);
        assert_eq!(later.uploaded, 0);
    }

    #[test]
    fn a_file_that_cannot_be_decoded_is_attempted_once_and_then_left_alone() {
        // The difference between one decode attempt and sixty a second, for a
        // file that will never work.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("broken.png");
        std::fs::write(&path, b"not a png").expect("writes");
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();
        let catalogue = catalogue(vec![located(clip, &path)]);

        let total = run_until(
            &mut residency,
            &loader,
            &catalogue,
            &[clip],
            &mut gpu,
            |residency| residency.has_failed(clip),
        );
        assert_eq!(total.requested, 1);
        assert_eq!(total.failed, 1);

        for _ in 0..60 {
            let report = residency.update(&loader, &catalogue, &[clip], &mut gpu, Instant::now());
            assert_eq!(report.requested, 0, "a broken file is not retried");
        }
        assert!(!residency.is_resident(clip));
        assert_eq!(gpu.texture_count(), 0);
    }

    #[test]
    fn re_importing_broken_media_clears_the_failure() {
        // A new asset is a new file, so the catalogue naming a different path is
        // how "the operator fixed it" reaches the renderer. Without this, a
        // broken import would need a renderer restart to recover from.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let broken = directory.path().join("broken.png");
        std::fs::write(&broken, b"not a png").expect("writes");
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();

        let first = catalogue(vec![located(clip, &broken)]);
        run_until(
            &mut residency,
            &loader,
            &first,
            &[clip],
            &mut gpu,
            |residency| residency.has_failed(clip),
        );

        let fixed = png(directory.path(), "fixed.png", [4, 5, 6, 255]);
        let second = catalogue(vec![located(clip, &fixed)]);
        run_until(
            &mut residency,
            &loader,
            &second,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );

        assert!(!residency.has_failed(clip));
        assert_eq!(gpu.texture(clip), Some((SizePx::new(2, 2), [4, 5, 6, 255])));
    }

    #[test]
    fn a_new_file_for_the_same_item_replaces_what_is_on_the_gpu() {
        // Re-transcoding media produces a new asset under a new key. The pixels
        // on the GPU are then the old ones, and nothing about the item's id
        // would say so.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let clip = MediaItemId::new();
        let old = png(directory.path(), "v1.png", [255, 0, 0, 255]);
        let new = png(directory.path(), "v2.png", [0, 0, 255, 255]);

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();

        let first = catalogue(vec![located(clip, &old)]);
        run_until(
            &mut residency,
            &loader,
            &first,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );
        assert_eq!(
            gpu.texture(clip),
            Some((SizePx::new(2, 2), [255, 0, 0, 255]))
        );

        let second = catalogue(vec![located(clip, &new)]);
        run_until(
            &mut residency,
            &loader,
            &second,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );
        assert_eq!(
            gpu.texture(clip),
            Some((SizePx::new(2, 2), [0, 0, 255, 255])),
            "the new file, not the old one"
        );
    }

    #[test]
    fn media_the_catalogue_no_longer_offers_is_freed() {
        // Loading another show must give its memory back, or a server left
        // running through a festival would fill its GPU one show at a time.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [7, 7, 7, 255]);
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();

        let loaded = catalogue(vec![located(clip, &path)]);
        run_until(
            &mut residency,
            &loader,
            &loaded,
            &[clip],
            &mut gpu,
            |residency| residency.is_resident(clip),
        );
        assert_eq!(gpu.texture_count(), 1);

        let empty = MediaCatalogue::new();
        let report = residency.update(&loader, &empty, &[], &mut gpu, Instant::now());

        assert_eq!(report.evicted, 1);
        assert_eq!(gpu.texture_count(), 0);
        assert!(!residency.is_resident(clip));
    }

    #[test]
    fn an_item_the_catalogue_cannot_resolve_is_not_asked_for() {
        // The ordinary case while media is still importing. Asking would mean a
        // decode request per frame for a file the core has not offered.
        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();
        let empty = MediaCatalogue::new();
        let clip = MediaItemId::new();

        for _ in 0..10 {
            let report = residency.update(&loader, &empty, &[clip], &mut gpu, Instant::now());
            assert_eq!(report.requested, 0);
        }
        assert!(!residency.is_resident(clip));
        assert!(!residency.has_failed(clip));
    }

    #[test]
    fn an_output_that_refuses_every_upload_is_not_asked_again_every_frame() {
        // A full GPU. The layer draws flat, which is visibly wrong, but the
        // renderer does not spend the rest of the show decoding the same file.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "clip.png", [9, 9, 9, 255]);
        let clip = MediaItemId::new();

        let mut gpu = HeadlessCompositor::new().refusing_uploads();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();
        let catalogue = catalogue(vec![located(clip, &path)]);

        run_until(
            &mut residency,
            &loader,
            &catalogue,
            &[clip],
            &mut gpu,
            |residency| residency.has_failed(clip),
        );

        for _ in 0..30 {
            let report = residency.update(&loader, &catalogue, &[clip], &mut gpu, Instant::now());
            assert_eq!(report.requested, 0);
        }
        assert_eq!(gpu.texture_count(), 0);
    }

    #[test]
    fn past_the_limit_the_least_recently_wanted_is_the_one_freed() {
        // Bounded GPU memory, and the bound is decided here rather than by the
        // driver refusing an allocation in the middle of a show.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();

        let mut ids = Vec::new();
        let mut entries = Vec::new();
        for index in 0..=MAX_RESIDENT {
            let path = png(
                directory.path(),
                &format!("{index}.png"),
                [u8::try_from(index % 256).expect("small"), 0, 0, 255],
            );
            let id = MediaItemId::new();
            ids.push(id);
            entries.push(located(id, &path));
        }
        let catalogue = catalogue(entries);

        // Wanted one at a time, oldest first, so the eviction order is known.
        for &id in &ids {
            run_until(
                &mut residency,
                &loader,
                &catalogue,
                &[id],
                &mut gpu,
                |residency| residency.is_resident(id) || residency.has_failed(id),
            );
        }

        assert_eq!(residency.resident_count(), MAX_RESIDENT);
        assert!(
            !residency.is_resident(ids[0]),
            "the one wanted longest ago is the one that went"
        );
        assert!(
            residency.is_resident(ids[MAX_RESIDENT]),
            "and the newest is still there"
        );
        assert_eq!(gpu.texture_count(), MAX_RESIDENT);
    }

    #[test]
    fn something_on_screen_is_never_the_thing_evicted() {
        // Evicting what the scene is asking for would free it and immediately
        // re-request it: a decode per frame, for as long as the show stayed
        // over the limit.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut gpu = HeadlessCompositor::new();
        let loader = MediaLoader::spawn().expect("starts");
        let mut residency = Residency::new();

        let mut ids = Vec::new();
        let mut entries = Vec::new();
        for index in 0..=MAX_RESIDENT {
            let path = png(directory.path(), &format!("{index}.png"), [0, 0, 0, 255]);
            let id = MediaItemId::new();
            ids.push(id);
            entries.push(located(id, &path));
        }
        let catalogue = catalogue(entries);

        // The first item stays on screen throughout.
        let on_screen = ids[0];
        for &id in &ids {
            let wanted = [on_screen, id];
            run_until(
                &mut residency,
                &loader,
                &catalogue,
                &wanted,
                &mut gpu,
                |residency| residency.is_resident(id),
            );
        }

        assert!(residency.is_resident(on_screen), "it was never off screen");
        assert_eq!(residency.resident_count(), MAX_RESIDENT);
    }
}
