//! Cover images for the UI: decoded off the UI thread, cached in memory (LRU, by
//! bytes), and handed to lazily-filled models.
//!
//! A model asks for a cover in `row_data`; if it is not in memory yet the row gets a
//! placeholder and the request goes to a worker. When the decode lands back on the UI
//! thread, every live model waiting for that cover gets a `row_changed`, which reaches the
//! tile of that row alone (`RowNotify`). This keeps scrolling smooth: nothing decodes on
//! the UI thread, only rows on screen (which `ListView` instantiates) ask for anything,
//! and a cover that loads redraws one tile.
//!
//! The newest request is decoded first. After a fast scroll through a large grid, the
//! covers where the scroll stopped come first, not after hundreds the user scrolled
//! past; and thumbnail requests beyond `MAX_WAITING` (the oldest, so long scrolled
//! past) are dropped. Rows that asked for a dropped cover get a `row_changed` too: a
//! row still on screen asks again (now the newest), one that is gone does nothing.
//! One-off images (`with_image`: the album page, the player bar, the backdrop) go first
//! and are never dropped.
//!
//! The covers of the grids and shelves are the art cache's thumbnails (`THUMB_PX`), as long
//! as that is as large as they are drawn. Zoomed in further (`zoom.rs`: a 184 px card at
//! 200% on a 150% display is 552 physical pixels), they are decoded from the full-size image
//! at the size drawn instead (`Kind::Card`, `set_card_px`), so they stay sharp.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex};

use library::{Album, ArtCache};
use slint::{Image, Model, ModelNotify, ModelPeer, ModelTracker, Rgb8Pixel, SharedPixelBuffer, SharedString};

use crate::AlbumCard;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Kind {
    Thumb,
    Full,
    Blur,
    /// A grid's or shelf's cover larger than the thumbnail: the full image, made `card_px`
    /// on its longest side (counted with the thumbnails).
    Card,
}

impl Kind {
    /// Thumbnails and cards share a budget and the queue that drops what was scrolled past.
    fn small(self) -> bool {
        matches!(self, Kind::Thumb | Kind::Card)
    }
}

/// Memory for decoded thumbnails (RGB, 3 bytes a pixel): ~260 of them at 360 px, a
/// few screens of a 4K grid. Images on screen hold their own reference, so eviction
/// never blanks anything visible; it only means a decode if a row asks again.
const THUMB_BUDGET: usize = 96 << 20;
/// Three 1200 px RGB covers and their 192 px backdrops fit after the LRU trim.
/// The album page and Now Playing retain their own references to displayed images.
const LARGE_BUDGET: usize = 16 << 20;
/// Thumbnail requests kept waiting at most; older ones were scrolled past.
const MAX_WAITING: usize = 160;
const WORKERS: usize = 3;

pub trait CoverListener {
    fn cover_ready(&self, stem: &str, kind: Kind);
    /// The request for this cover was dropped (scrolled past): rows still on screen
    /// that want it should ask again.
    fn cover_dropped(&self, _stem: &str) {}
    /// This cover could not be opened (its file gone or damaged): it is not asked for again.
    fn cover_failed(&self, _stem: &str, _kind: Kind) {}
    /// The grids' covers are wanted at another size (`set_card_px`): rows on screen ask
    /// again.
    fn cards_resized(&self) {}
    /// One-off listeners return true once they have fired, and are then dropped.
    fn finished(&self) -> bool {
        false
    }
}

struct Entry {
    image: Image,
    bytes: usize,
    last_used: u64,
}

type Key = (String, Kind);

struct Job {
    key: Key,
    path: PathBuf,
    /// A card: the thumbnail, should the full image not open, and the longest side wanted.
    card: Option<(PathBuf, u32)>,
}

/// Decode requests, newest last. `first` (one-off images) is served before `thumbs`.
#[derive(Default)]
struct Jobs {
    first: Vec<Job>,
    thumbs: Vec<Job>,
}

type Queue = Arc<(Mutex<Jobs>, Condvar)>;

struct Cache {
    art: ArtCache,
    entries: HashMap<Key, Entry>,
    /// Bytes held, thumbnails and the rest.
    thumb_bytes: usize,
    large_bytes: usize,
    pending: HashSet<Key>,
    /// Covers that failed to decode; not retried this session.
    failed: HashSet<Key>,
    tick: u64,
    listeners: Vec<Weak<dyn CoverListener>>,
    queue: Queue,
    /// The grids' covers' size, physical pixels, when larger than a thumbnail (0: thumbnails).
    card_px: u32,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

/// Start the decode workers. Call once, on the UI thread.
pub fn init(art: ArtCache) {
    let queue: Queue = Arc::default();
    for n in 0..WORKERS {
        let queue = queue.clone();
        std::thread::Builder::new()
            .name(format!("mp-covers-{n}"))
            .spawn(move || {
                // Below the UI and audio threads: decoding covers can wait a moment.
                library::background_thread();
                loop {
                    let job = {
                        let (lock, ready) = &*queue;
                        let mut jobs = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(j) = jobs.first.pop().or_else(|| jobs.thumbs.pop()) {
                                break j;
                            }
                            jobs = ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let opened = match &job.card {
                        Some((thumb, px)) => image::open(&job.path).or_else(|_| image::open(thumb)).map(|img| {
                            if img.width().max(img.height()) > *px { img.thumbnail(*px, *px) } else { img }
                        }),
                        None => image::open(&job.path),
                    };
                    let decoded = opened.ok().map(|img| {
                        let rgb = img.into_rgb8();
                        SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(rgb.as_raw(), rgb.width(), rgb.height())
                    });
                    #[cfg(feature = "bench")]
                    DECODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let (stem, kind) = job.key;
                    let _ = slint::invoke_from_event_loop(move || deliver(stem, kind, decoded));
                }
            })
            .expect("spawn cover worker");
    }
    CACHE.with(|c| {
        *c.borrow_mut() = Some(Cache {
            art,
            entries: HashMap::new(),
            thumb_bytes: 0,
            large_bytes: 0,
            pending: HashSet::new(),
            failed: HashSet::new(),
            tick: 0,
            listeners: Vec::new(),
            queue,
            card_px: 0,
        })
    });
}

pub fn register(listener: Weak<dyn CoverListener>) {
    CACHE.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            c.listeners.retain(|l| l.strong_count() > 0);
            c.listeners.push(listener);
        }
    });
}

/// The image if it is in memory; otherwise `None` and a load is queued (or, if it is
/// already queued, moved to the front: the newest request is served first).
pub fn get(stem: &str, kind: Kind) -> Option<Image> {
    request(stem, kind, false)
}

/// `get`, for a one-off image: served before thumbnails and never dropped.
fn request(stem: &str, kind: Kind, keep: bool) -> Option<Image> {
    let dropped = CACHE.with(|c| {
        let mut guard = c.borrow_mut();
        let c = guard.as_mut()?;
        c.tick += 1;
        let tick = c.tick;
        let key = (stem.to_owned(), kind);
        if let Some(e) = c.entries.get_mut(&key) {
            e.last_used = tick;
            return Some(Ok(e.image.clone()));
        }
        if c.failed.contains(&key) {
            return None;
        }
        let first = keep || !kind.small();
        let (lock, ready) = &*c.queue;
        let mut jobs = lock.lock().unwrap_or_else(|e| e.into_inner());
        if !c.pending.insert(key.clone()) {
            // Already asked for: to the front, unless a worker has it already.
            let queued = jobs.thumbs.iter().position(|j| j.key == key).map(|i| jobs.thumbs.remove(i));
            let queued = queued.or_else(|| jobs.first.iter().position(|j| j.key == key).map(|i| jobs.first.remove(i)));
            match queued {
                Some(job) if first => jobs.first.push(job),
                Some(job) => jobs.thumbs.push(job),
                None => {}
            }
            return None;
        }
        let paths = c.art.paths(stem);
        let (path, card) = match kind {
            Kind::Thumb => (paths.thumb, None),
            Kind::Full => (paths.full, None),
            Kind::Blur => (paths.blur, None),
            Kind::Card => (paths.full, Some((paths.thumb, c.card_px.max(library::art::THUMB_PX)))),
        };
        if first {
            jobs.first.push(Job { key, path, card });
        } else {
            jobs.thumbs.push(Job { key, path, card });
        }
        ready.notify_one();
        // Too many waiting: the oldest were scrolled past long ago.
        let excess = jobs.thumbs.len().saturating_sub(MAX_WAITING);
        let dropped: Vec<Key> = jobs.thumbs.drain(..excess).map(|j| j.key).collect();
        drop(jobs);
        for key in &dropped {
            c.pending.remove(key);
        }
        Some(Err(dropped.into_iter().map(|k| k.0).collect::<Vec<String>>()))
    });
    match dropped? {
        Ok(image) => Some(image),
        Err(dropped) => {
            if !dropped.is_empty() {
                // Later, not inside the `row_data` that asked: listeners notify models.
                let _ = slint::invoke_from_event_loop(move || notify_dropped(&dropped));
            }
            None
        }
    }
}

fn notify_dropped(stems: &[String]) {
    let listeners = CACHE.with(|c| {
        c.borrow().as_ref().map(|c| c.listeners.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()).unwrap_or_default()
    });
    for l in listeners {
        for stem in stems {
            l.cover_dropped(stem);
        }
    }
}

fn deliver(stem: String, kind: Kind, decoded: Option<SharedPixelBuffer<Rgb8Pixel>>) {
    let failed = decoded.is_none();
    let listeners = CACHE.with(|c| {
        let mut guard = c.borrow_mut();
        let Some(c) = guard.as_mut() else { return Vec::new() };
        let key = (stem.clone(), kind);
        c.pending.remove(&key);
        let Some(buf) = decoded else {
            c.failed.insert(key);
            return c.listeners.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
        };
        c.tick += 1;
        let bytes = buf.width() as usize * buf.height() as usize * 3;
        let thumb = kind.small();
        if let Some(old) = c.entries.insert(key.clone(), Entry { image: Image::from_rgb8(buf), bytes, last_used: c.tick }) {
            if thumb { c.thumb_bytes -= old.bytes } else { c.large_bytes -= old.bytes }
        }
        if thumb { c.thumb_bytes += bytes } else { c.large_bytes += bytes }
        evict(c, thumb, &key);
        c.listeners.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
    });
    // Outside the borrow: listeners call back into `get`.
    for l in listeners {
        if failed { l.cover_failed(&stem, kind) } else { l.cover_ready(&stem, kind) }
    }
    HOLD.with(|h| h.borrow_mut().retain(|l| !l.finished()));
}

/// Over budget: drop the least recently used images of that class down to 7/8 of it
/// (in one sort, not one scan per image), never the one just added.
fn evict(c: &mut Cache, thumb: bool, keep: &Key) {
    let budget = if thumb { THUMB_BUDGET } else { LARGE_BUDGET };
    let held = if thumb { c.thumb_bytes } else { c.large_bytes };
    if held <= budget {
        return;
    }
    let mut olds: Vec<(u64, Key, usize)> = c
        .entries
        .iter()
        .filter(|(k, _)| k.1.small() == thumb && *k != keep)
        .map(|(k, e)| (e.last_used, k.clone(), e.bytes))
        .collect();
    olds.sort_by_key(|(t, _, _)| *t);
    let mut held = held;
    for (_, k, bytes) in olds {
        if held <= budget / 8 * 7 {
            break;
        }
        c.entries.remove(&k);
        held -= bytes;
    }
    if thumb { c.thumb_bytes = held } else { c.large_bytes = held }
}

/// What the grids and shelves ask for: thumbnails, or cards once they are drawn larger.
pub fn card_kind() -> Kind {
    CACHE.with(|c| c.borrow().as_ref().map_or(Kind::Thumb, |c| if c.card_px > 0 { Kind::Card } else { Kind::Thumb }))
}

/// The size cards are decoded at for covers drawn `px` physical pixels wide: none (0, the
/// thumbnails serve) up to a little more than a thumbnail, else `px` rounded up to a 60 px
/// step, so a small change does not decode everything again.
pub fn card_size(px: u32) -> u32 {
    let thumb = library::art::THUMB_PX;
    if px.saturating_mul(20) <= thumb * 21 { 0 } else { px.div_ceil(60) * 60 }
}

/// The grids' covers are drawn `px` physical pixels wide (`zoom.rs`, at each change of the
/// scale factor): see `card_size`. A new size drops the cards held, and the models on screen
/// ask again.
pub fn set_card_px(px: u32) {
    let want = card_size(px);
    let listeners = CACHE.with(|c| {
        let mut guard = c.borrow_mut();
        let c = guard.as_mut()?;
        if c.card_px == want {
            return None;
        }
        log::info!(
            "covers of the grids: {}",
            if want == 0 { format!("thumbnails ({} px)", library::art::THUMB_PX) } else { format!("{want} px from the full images") }
        );
        c.card_px = want;
        let mut freed = 0;
        c.entries.retain(|k, e| {
            let keep = k.1 != Kind::Card;
            if !keep {
                freed += e.bytes;
            }
            keep
        });
        c.thumb_bytes -= freed;
        c.failed.retain(|k| k.1 != Kind::Card);
        Some(c.listeners.iter().filter_map(Weak::upgrade).collect::<Vec<_>>())
    });
    for l in listeners.into_iter().flatten() {
        l.cards_resized();
    }
}

/// A stable placeholder colour for albums without art.
pub fn tint_for(title: &str) -> slint::Color {
    let mut h: u32 = 2166136261;
    for b in title.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32;
    // OKLCH at one lightness and a low chroma, so every placeholder sits at the same
    // perceived depth whatever its hue (HSL made greens and yellows read much lighter).
    let lab = library::accent::from_lch([0.36, 0.055, hue.to_radians()]);
    let [r, g, b] = library::accent::oklab_to_srgb(lab);
    slint::Color::from_rgb_u8(r, g, b)
}


pub fn card(a: &Album, cover: Option<Image>) -> AlbumCard {
    AlbumCard {
        id: a.id as i32,
        title: SharedString::from(a.title.as_str()),
        artist: SharedString::from(a.artist.as_str()),
        year: a.year().unwrap_or("").into(),
        has_cover: a.art.is_some(),
        cover: cover.unwrap_or_default(),
        tint: tint_for(&a.title),
        remote: a.remote_only,
    }
}

/// A model's change tracking with a tracker for each row read by its index.
///
/// A grid reads its tiles by index (`albums[i]`: Slint has no grid, so a row of the list
/// holds several). `ModelNotify` alone keeps one dependency for every row read that way: a
/// change to one row, a cover loaded, then has every tile on screen read its album again
/// and lay out its texts and its artist links again, on each frame a cover came in.
/// Scrolling fast through covers not in memory yet, that was every frame: on a maximized
/// 1440p window (sixty tiles) frames of 16 to 20 ms where the Genres page, whose few covers
/// are soon all in memory, took 5 (`MP_BENCH=grid`). Here a row's change reaches only what
/// read that row.
#[derive(Default)]
struct RowNotify {
    /// The repeaters over the model (`for album in albums`) and its row count.
    all: ModelNotify,
    /// The rows read by index, each with what depends on it (as row 0 of its own tracker).
    rows: RefCell<HashMap<usize, Rc<ModelNotify>>>,
}

impl RowNotify {
    fn row_changed(&self, row: usize) {
        // Out of the map first: marking dependents must not meet a borrow of it.
        let tracker = self.rows.borrow().get(&row).cloned();
        if let Some(t) = tracker {
            t.row_changed(0);
        }
        self.all.row_changed(row);
    }

    /// Rows came, went or moved: whatever read a row by its index reads it again (and
    /// tracks it again, so the trackers start over).
    fn every_row_changed(&self) {
        let rows = std::mem::take(&mut *self.rows.borrow_mut());
        for t in rows.values() {
            t.row_changed(0);
        }
    }

    fn row_added(&self, index: usize, count: usize) {
        self.every_row_changed();
        self.all.row_added(index, count);
    }

    fn row_removed(&self, index: usize, count: usize) {
        self.every_row_changed();
        self.all.row_removed(index, count);
    }

    fn reset(&self) {
        self.every_row_changed();
        self.all.reset();
    }
}

impl ModelTracker for RowNotify {
    fn attach_peer(&self, peer: ModelPeer) {
        self.all.attach_peer(peer);
    }

    fn track_row_count_changes(&self) {
        self.all.track_row_count_changes();
    }

    fn track_row_data_changes(&self, row: usize) {
        let tracker = self.rows.borrow_mut().entry(row).or_default().clone();
        tracker.track_row_data_changes(0);
    }
}

/// Albums as a Slint model, covers filled in as they load.
pub struct AlbumModel {
    albums: RefCell<Vec<Album>>,
    /// Rows waiting for each cover stem.
    waiting: RefCell<HashMap<String, Vec<usize>>>,
    notify: RowNotify,
}

impl AlbumModel {
    pub fn new(albums: Vec<Album>) -> Rc<Self> {
        let m = Rc::new(Self { albums: RefCell::new(albums), waiting: RefCell::default(), notify: RowNotify::default() });
        let weak: Weak<dyn CoverListener> = Rc::downgrade(&(m.clone() as Rc<dyn CoverListener>));
        register(weak);
        m
    }

    pub fn set(&self, albums: Vec<Album>) {
        *self.albums.borrow_mut() = albums;
        self.waiting.borrow_mut().clear();
        self.notify.reset();
    }

    /// Show `albums` with as little change on screen as can be: the same albums in the same
    /// places change only the tiles whose album changed (a cover came in), albums after the
    /// last are added after it, the last ones taken away when the list is shorter (a grid
    /// shelf with fewer columns), and only anything else starts the list again (`set`). What
    /// a scan in progress brings to an open page (see `views::library_changed_live`).
    pub fn update(&self, albums: Vec<Album>) {
        let (changed, old_len) = {
            let old = self.albums.borrow();
            if old.iter().zip(&albums).any(|(a, b)| a.id != b.id) {
                drop(old);
                self.set(albums);
                return;
            }
            let changed: Vec<usize> = old.iter().zip(&albums).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
            (changed, old.len())
        };
        let new_len = albums.len();
        *self.albums.borrow_mut() = albums;
        if new_len < old_len {
            self.waiting.borrow_mut().values_mut().for_each(|rows| rows.retain(|&r| r < new_len));
            self.notify.row_removed(new_len, old_len - new_len);
        }
        for row in changed {
            self.notify.row_changed(row);
        }
        if new_len > old_len {
            self.notify.row_added(old_len, new_len - old_len);
        }
    }

    /// The albums shown, as they are now in `albums` (or `fresh` for one not among them), then
    /// those of `albums` not shown yet, up to `limit` in all: a list that only grows at its
    /// end, for a page open while a scan runs.
    pub fn grown(&self, albums: Vec<Album>, limit: usize, fresh: impl Fn(i64) -> Option<Album>) -> Vec<Album> {
        let old = self.albums.borrow();
        let mut by_id: std::collections::HashMap<i64, Album> = albums.iter().map(|a| (a.id, a.clone())).collect();
        let mut out: Vec<Album> = old.iter().map(|a| by_id.remove(&a.id).or_else(|| fresh(a.id)).unwrap_or_else(|| a.clone())).collect();
        let shown: std::collections::HashSet<i64> = out.iter().map(|a| a.id).collect();
        for a in albums {
            if out.len() >= limit {
                break;
            }
            if !shown.contains(&a.id) {
                out.push(a);
            }
        }
        out
    }

    pub fn ids(&self) -> Vec<i64> {
        self.albums.borrow().iter().map(|a| a.id).collect()
    }
}

impl Model for AlbumModel {
    type Data = AlbumCard;

    fn row_count(&self) -> usize {
        self.albums.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<AlbumCard> {
        #[cfg(feature = "bench")]
        ROW_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let albums = self.albums.borrow();
        let a = albums.get(row)?;
        // An album only the server has, on screen without its cover yet: fetched first.
        if a.art.is_none() && a.remote_only && a.remote_art.is_some() {
            crate::server::want_cover(a.id);
        }
        let cover = a.art.as_deref().and_then(|stem| {
            let img = get(stem, card_kind());
            if img.is_none() {
                // Once: a row read again while it waits is told once when the cover comes.
                let mut waiting = self.waiting.borrow_mut();
                let rows = waiting.entry(stem.to_owned()).or_default();
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
            img
        });
        Some(card(a, cover))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

impl CoverListener for AlbumModel {
    fn cover_ready(&self, stem: &str, kind: Kind) {
        if kind != card_kind() {
            return;
        }
        let rows = self.waiting.borrow_mut().remove(stem);
        for r in rows.into_iter().flatten() {
            if r < self.albums.borrow().len() {
                self.notify.row_changed(r);
            }
        }
    }

    fn cover_dropped(&self, stem: &str) {
        // Rows on screen ask again; the rest do nothing.
        self.cover_ready(stem, card_kind());
    }

    fn cards_resized(&self) {
        // Every row: only those on screen ask again (row_data), the rest when they come.
        self.waiting.borrow_mut().clear();
        for r in 0..self.albums.borrow().len() {
            self.notify.row_changed(r);
        }
    }
}

/// Run `f` with the image once it is loaded (immediately if cached). For one-off
/// images: the album page cover, the backdrop, the player bar.
pub fn with_image(stem: &str, kind: Kind, f: impl Fn(Image) + 'static) {
    if let Some(img) = request(stem, kind, true) {
        f(img);
        return;
    }
    // Known not to open: nothing will come.
    if CACHE.with(|c| c.borrow().as_ref().is_some_and(|c| c.failed.contains(&(stem.to_owned(), kind)))) {
        return;
    }
    struct Once<F: Fn(Image)> {
        stem: String,
        kind: Kind,
        f: RefCell<Option<F>>,
    }
    impl<F: Fn(Image)> CoverListener for Once<F> {
        fn cover_ready(&self, stem: &str, kind: Kind) {
            if stem == self.stem
                && kind == self.kind
                && let Some(f) = self.f.borrow_mut().take()
                && let Some(img) = get(stem, kind)
            {
                f(img);
            }
        }
        fn cover_failed(&self, stem: &str, kind: Kind) {
            if stem == self.stem && kind == self.kind {
                self.f.borrow_mut().take();
            }
        }
        fn finished(&self) -> bool {
            self.f.borrow().is_none()
        }
    }
    let once: Rc<dyn CoverListener> = Rc::new(Once { stem: stem.to_owned(), kind, f: RefCell::new(Some(f)) });
    register(Rc::downgrade(&once));
    // Held until it fires (see `deliver`); the registry only has a weak reference.
    HOLD.with(|h| h.borrow_mut().push(once));
}

thread_local! {
    static HOLD: RefCell<Vec<Rc<dyn CoverListener>>> = const { RefCell::new(Vec::new()) };
}

/// Measurement (bench/mod.rs): decodes finished so far, and whether a cover is in memory.
#[cfg(feature = "bench")]
static DECODES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "bench")]
pub fn decodes() -> usize {
    DECODES.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(feature = "bench")]
pub fn cached(stem: &str, kind: Kind) -> bool {
    CACHE.with(|c| c.borrow().as_ref().is_some_and(|c| c.entries.contains_key(&(stem.to_owned(), kind))))
}

/// Measurement (bench/mod.rs): rows of album models read so far. A grid scrolled reads each
/// tile's a few times (as it comes, and again when its cover does), not once for every
/// cover that comes in while it is on screen (`RowNotify`).
#[cfg(feature = "bench")]
static ROW_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "bench")]
pub fn row_reads() -> usize {
    ROW_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Measurement (bench/zoom.rs): the size of a cover in memory.
#[cfg(feature = "bench")]
pub fn cached_size(stem: &str, kind: Kind) -> Option<(u32, u32)> {
    CACHE.with(|c| c.borrow().as_ref().and_then(|c| c.entries.get(&(stem.to_owned(), kind)).map(|e| (e.image.size().width, e.image.size().height))))
}

impl AlbumModel {
    /// Measurement (bench/mod.rs): the art stem of a row, without asking for its cover.
    pub fn art_at(&self, row: usize) -> Option<Option<String>> {
        self.albums.borrow().get(row).map(|a| a.art.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-off image that cannot be opened (its file gone, damaged): its listener goes, and one
    /// asked for after that is not held at all.
    #[test]
    fn an_image_that_fails_holds_no_listener() {
        let dir = std::env::temp_dir().join(format!("mp-covers-failed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        init(ArtCache::new(&dir));
        let held = || HOLD.with(|h| h.borrow().len());
        let listeners = || CACHE.with(|c| c.borrow().as_ref().map_or(0, |c| c.listeners.iter().filter(|l| l.strong_count() > 0).count()));
        with_image("gone", Kind::Full, |_| panic!("no image"));
        assert_eq!((held(), listeners()), (1, 1), "waiting for its load");
        deliver("gone".into(), Kind::Full, None);
        assert_eq!((held(), listeners()), (0, 0), "dropped once it failed");
        with_image("gone", Kind::Full, |_| panic!("no image"));
        assert_eq!((held(), listeners()), (0, 0), "known to fail: not held");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A grid shelf with fewer columns: the first albums kept in place, the rest taken off the
    /// end; more columns: added after them; another order: the list again.
    #[test]
    fn an_album_list_shrinks_and_grows_at_its_end() {
        let albums = |ids: &[i64]| ids.iter().map(|&id| Album { id, ..Default::default() }).collect::<Vec<_>>();
        let m = AlbumModel::new(albums(&[1, 2, 3, 4, 5, 6]));
        m.waiting.borrow_mut().insert("a".into(), vec![1, 5]);
        m.update(albums(&[1, 2, 3, 4]));
        assert_eq!((m.ids(), m.row_count()), (vec![1, 2, 3, 4], 4));
        assert_eq!(m.waiting.borrow().get("a"), Some(&vec![1]), "no row past the end waits for a cover");
        m.update(albums(&[1, 2, 3, 4, 7, 8]));
        assert_eq!(m.ids(), vec![1, 2, 3, 4, 7, 8]);
        m.update(albums(&[2, 1]));
        assert_eq!(m.ids(), vec![2, 1]);
    }
}
