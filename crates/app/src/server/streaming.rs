//! Tracks only the server has, played as they download, and the cache their files are kept in.
//!
//! **What is fetched.** The original file or an MP3 subject to the configured bitrate cap, into
//! the stream cache (`Paths::stream`: `%LOCALAPPDATA%\
//! MarqueePlayer\stream`, `~/.cache/marquee-player/stream`, `cache\stream` under `MP_DATA_DIR`).
//! Only the queue's current track, its next one (the engine's gapless next, `playback::sync_next`)
//! and one more after those are fetched; nothing is fetched for a track merely listed on a page.
//! The client is used for reads only: nothing is written to the server here.
//!
//! **One file per representation**, named by the song id, its size in the catalog (`key`) and,
//! for MP3 transcodes, the bitrate. A change of source size invalidates every version.
//! The catalog hash changes with the server's play counts and stars, so it isn't used. A file
//! being written has a sidecar beside it (`<key>.partial`: its length and the stretches on disk),
//! made before the file and removed only once the whole file is on disk and synced, so a partial
//! file is never taken for a whole one, after a crash too: a later request goes on from the
//! stretches the sidecar lists for an original. An interrupted transcode restarts from the beginning
//! because separate encoder runs can produce different bytes. An original without a
//! sidecar whose size is not the catalog's is deleted. A whole file is played as any local file
//! (`TrackRequest::new`): no network at all.
//!
//! **A file replaced on the server** under the same id and with the same size is told apart only
//! by the server's answers: the sidecar keeps the first answer's validator (`StreamHead::
//! validator`: Navidrome sends the file's `Last-Modified`), each range asked later sends it as
//! `If-Range`, and the server sends a changed file whole from its start (200) instead of a range of
//! it. A partial file going on from a sidecar shows the engine none of its stretches until the
//! first answer says they are of the file the server has; when it says otherwise, the partial
//! file is emptied and the new one written from what came, never stitched to the old. Within one
//! download (the engine has read the old bytes) the feed fails as for a change of size. A whole
//! cached file is not checked: the catalog has no mark of a file's version (its `created` may stay
//! when a file is replaced, its hash moves with play counts and stars), and asking the server on
//! each play would cost the request a cached play saves.
//!
//! **A download** is a thread of its own (`mp-stream`), which writes the file and tells the
//! engine's [`Feed`] what is on disk once it is written (plain `write`s to the file, which another
//! handle reads at once; the sidecar only after `sync_data`). It reads from the start; when a
//! reader waits for an offset (`Feed::wanted`) behind what is being written or more than `JUMP`
//! ahead of it (an Ogg file's last page and an MP4's `moov` at the end, which open needs; the
//! bisection of a FLAC or Ogg seek; a seek past the download), it drops the connection and asks
//! for a range from there, then fills the gaps that are left, from where it is and round to the
//! start, until the file is whole. A connection that fails (no answer, a stall past the read
//! timeout, a drop mid-body, a 5xx) is retried after a pause (`BACKOFF`), from where it stopped;
//! after `ATTEMPTS` failures in a row with nothing written between them, or at once on an error
//! that retrying cannot mend (the file gone, the sign-in refused), the feed fails with words for
//! the pill (`words`) and the engine moves on as for a damaged file. It stops at the next chunk
//! when it is not wanted any more (the engine has no reader on it and it is none of the tracks
//! kept, `keep`), on sign-out (the feed fails: "signed out of the server") and at exit; a stalled
//! connection ends at its read timeout. What it wrote stays, for a later request to go on from.
//!
//! **The cap** (Settings > Playback, 2 GB by default): after each download and when the cap
//! changes, the least recently played files go first (a cached file's time is set when it is
//! played), never the current or next track's nor one being written. Sign-out clears the cache
//! (`clear_cache`); Settings' Clear keeps only the tracks the queue is playing.
//!
//! The app reaches this through the functions at the end (`install`, `set_client`, `request`,
//! `keep`, ...), which drive one [`Streamer`]; tests and the bench build their own.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use library::remote::StreamInfo;
use player_core::{Feed, TrackRequest};
use serde::{Deserialize, Serialize};
use subsonic::{StreamHead, StreamSink};

use super::provider::Client;

/// The cache's size by default: 2 GB (a few hundred lossless tracks; `DEFAULT_CAP_MB` in the
/// settings).
#[cfg(any(test, feature = "bench"))]
pub const DEFAULT_CAP: u64 = DEFAULT_CAP_MB * MB;
/// A reader waiting this far ahead of the download (or anywhere behind it) gets a range request
/// there; nearer ahead, the download reaches it soon enough by itself.
const JUMP: u64 = 256 * 1024;
/// The sidecar is written at most this often (after `sync_data`), and when a download stops.
const PERSIST_EVERY: Duration = Duration::from_secs(1);
/// The pauses before each retry of a failed connection; then the last, until `ATTEMPTS`.
const BACKOFF: [Duration; 5] = [Duration::from_millis(500), Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(4), Duration::from_secs(8)];
/// Failures in a row, with nothing written between them, before a download gives up.
const ATTEMPTS: usize = 6;
/// Each read of a song's body: a stall is a failure after this (then retried). Shorter than the
/// client's default (30 s), which a server starting a transcode may need.
const READ_TIMEOUT_MS: u32 = 15_000;

/// What `request` cannot do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No signed-in client (signed out, or not connected yet), and the file is not cached.
    NoClient,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoClient => f.write_str("not signed in to the server"),
        }
    }
}

/// What a failed stream says in the pill ("Can't play “X”: <words>").
pub fn words(e: &subsonic::Error) -> String {
    use subsonic::Error as E;
    match e {
        E::Network(_) => "the server cannot be reached".to_owned(),
        E::NotFound(_) => "the file is no longer on the server".to_owned(),
        E::WrongCredentials { .. } => "the server refused the sign-in (sign in again in Settings > Library)".to_owned(),
        E::NotPermitted(_) => "the server does not let this account play it".to_owned(),
        E::Server { http_status: Some(s), .. } => format!("the server failed (HTTP {s})"),
        E::Server { code: Some(c), message, .. } if !message.is_empty() => format!("the server failed (error {c}: {message})"),
        other => other.to_string(),
    }
}

pub const SIGNED_OUT: &str = "signed out of the server";

/// How a [`Streamer`] is set up.
#[derive(Clone)]
pub struct Config {
    pub dir: PathBuf,
    pub cap: u64,
    pub read_timeout_ms: u32,
    pub backoff: Vec<Duration>,
    pub attempts: usize,
    /// Called (on the worker thread that did it) when the space used may have changed: a download
    /// finished, files were evicted or cleared. With the bytes the cache holds, counted on that
    /// thread, so whoever shows them needs no look at the folder.
    pub changed: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}

impl Config {
    pub fn new(dir: PathBuf, cap: u64) -> Self {
        Config { dir, cap, read_timeout_ms: READ_TIMEOUT_MS, backoff: BACKOFF.to_vec(), attempts: ATTEMPTS, changed: None }
    }
}

// ---- names on disk -------------------------------------------------------------------

/// A song's name in the cache: its id (made safe for a file name) and its size in the catalog.
pub fn key(info: &StreamInfo) -> String {
    let safe = info.song_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !info.song_id.is_empty() && info.song_id.len() <= 64;
    let id = if safe { info.song_id.clone() } else { format!("x{:016x}", fnv(info.song_id.as_bytes())) };
    format!("{id}-{}", info.size.max(0))
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &c| (h ^ u64::from(c)).wrapping_mul(0x100_0000_01b3))
}

/// The file's extension: the catalog's suffix when it is a plain one (the probe goes by it).
fn extension(info: &StreamInfo) -> String {
    match info.suffix.as_deref().map(str::to_ascii_lowercase) {
        Some(s) if !s.is_empty() && s.len() <= 5 && s.chars().all(|c| c.is_ascii_alphanumeric()) => s,
        _ => "audio".to_owned(),
    }
}

/// Names and size checks for one representation of a catalog track.
#[derive(Clone)]
struct CachedFile {
    key: String,
    path: PathBuf,
    bitrate: Option<u32>,
    expected: u64,
}

impl CachedFile {
    fn new(dir: &Path, info: &StreamInfo, bitrate: Option<u32>) -> Self {
        let original = key(info);
        let (key, ext, expected) = match bitrate {
            Some(kbps) => (format!("{original}-mp3-{kbps}k"), "mp3".to_owned(), 0),
            None => (original, extension(info), info.size.max(0) as u64),
        };
        let path = dir.join(format!("{key}.{ext}"));
        Self { key, path, bitrate, expected }
    }

    fn discard_partial(&self, sidecar: &Path) {
        if remove_if_present(&self.path).is_ok() {
            let _ = fs::remove_file(sidecar);
        }
    }

    fn whole(&self) -> bool {
        if self.path.with_extension("partial").exists() {
            return false;
        }
        let Ok(meta) = fs::metadata(&self.path) else { return false };
        if !meta.is_file() {
            return false;
        }
        if meta.len() == 0 || (self.expected > 0 && meta.len() != self.expected) {
            log::warn!("stream cache: {} has an invalid length: deleted", self.path.display());
            let _ = fs::remove_file(&self.path);
            return false;
        }
        true
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn sidecar_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.partial"))
}

/// The key a file in the cache belongs to (the name before its first dot).
fn key_of(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// Mark a cached file as just played (eviction goes by it).
fn touch(path: &Path) {
    if let Ok(f) = OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

// ---- the stretches on disk -----------------------------------------------------------

/// Add `r` to sorted, disjoint, non-touching stretches.
fn add_range(filled: &mut Vec<Range<u64>>, r: Range<u64>) {
    if r.start >= r.end {
        return;
    }
    let (mut start, mut end) = (r.start, r.end);
    filled.retain(|f| {
        if f.end < start || f.start > end {
            return true;
        }
        start = start.min(f.start);
        end = end.max(f.end);
        false
    });
    let at = filled.partition_point(|f| f.start < start);
    filled.insert(at, start..end);
}

/// The stretches of `0..total` not in `filled`.
fn gaps(filled: &[Range<u64>], total: u64) -> Vec<Range<u64>> {
    let mut out = Vec::new();
    let mut at = 0;
    for f in filled {
        if f.start > at {
            out.push(at..f.start.min(total));
        }
        at = at.max(f.end);
    }
    if at < total {
        out.push(at..total);
    }
    out.retain(|g| g.start < g.end);
    out
}

/// Where to read next: the gap a reader waits in (from its offset), else the first gap at or
/// after `cursor`, else the first gap of all.
fn next_stretch(filled: &[Range<u64>], total: u64, wanted: Option<u64>, cursor: u64) -> Option<Range<u64>> {
    let g = gaps(filled, total);
    if let Some(w) = wanted
        && let Some(gap) = g.iter().find(|r| r.start <= w && w < r.end)
    {
        return Some(w..gap.end);
    }
    g.iter().find(|r| r.end > cursor).map(|r| r.start.max(cursor)..r.end).or_else(|| g.first().cloned())
}

/// The sidecar: the file's length and its stretches on disk.
#[derive(Serialize, Deserialize, Default)]
struct Sidecar {
    size: u64,
    filled: Vec<(u64, u64)>,
    /// The validator of the file the stretches are of, when the server sent one: sent back as
    /// `If-Range`. None in a sidecar written before it was kept (then the stretches are trusted,
    /// as they were).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validator: Option<String>,
}

/// The file being written and what of it is on disk.
struct Disk {
    file: File,
    sidecar: PathBuf,
    filled: Vec<Range<u64>>,
    total: Option<u64>,
    /// What tells the file being written from another version of it (`StreamHead::validator`).
    validator: Option<String>,
    /// The stretches on disk are known to be of the file the server has, and the feed was told
    /// of them. False for stretches from an earlier run, kept under a validator, until an answer
    /// confirms it.
    confirmed: bool,
    persisted: Instant,
    dirty: bool,
}

impl Disk {
    /// Go on from a partial file when its sidecar reads and agrees with the catalog's size; else
    /// start afresh (the sidecar first, then the file: a crash in between leaves a sidecar, never
    /// a file that looks whole).
    fn open(file: &CachedFile, sidecar: &Path) -> io::Result<Disk> {
        let mut filled = Vec::new();
        let mut total = None;
        let mut validator = None;
        let path = &file.path;
        let kept = fs::read(sidecar).ok().and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok());
        match kept {
            Some(s) if file.bitrate.is_none() && s.size > 0 && (file.expected == 0 || s.size == file.expected) => {
                let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                for (a, b) in s.filled {
                    // Only what the file holds: a stretch past its end was not written.
                    add_range(&mut filled, a.min(len)..b.min(len).min(s.size));
                }
                total = Some(s.size);
                validator = s.validator;
                log::info!("stream cache: going on with {} ({} of {} bytes on disk)", path.display(), filled.iter().map(|r| r.end - r.start).sum::<u64>(), s.size);
            }
            _ => remove_if_present(path)?,
        }
        write_sidecar(sidecar, total.unwrap_or(0), &filled, validator.as_deref())?;
        let persisted = Instant::now();
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let confirmed = validator.is_none() || filled.is_empty();
        Ok(Disk { file, sidecar: sidecar.to_path_buf(), filled, total, validator, confirmed, persisted, dirty: false })
    }

    /// Another version of the file than the stretches on disk: the partial file emptied (the
    /// sidecar first), to be written afresh from what the server sends now.
    fn restart(&mut self, total: Option<u64>, validator: Option<String>) -> io::Result<()> {
        self.filled.clear();
        self.total = total;
        self.validator = validator;
        self.confirmed = true;
        self.write_sidecar()?;
        self.file.set_len(0)
    }

    fn write_at(&mut self, pos: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(pos))?;
        self.file.write_all(bytes)?;
        add_range(&mut self.filled, pos..pos + bytes.len() as u64);
        self.dirty = true;
        Ok(())
    }

    fn whole(&self) -> bool {
        self.total.is_some_and(|t| t == 0 || self.filled.first().is_some_and(|r| r.start == 0 && r.end >= t))
    }

    fn write_sidecar(&mut self) -> io::Result<()> {
        write_sidecar(&self.sidecar, self.total.unwrap_or(0), &self.filled, self.validator.as_deref())?;
        self.persisted = Instant::now();
        self.dirty = false;
        Ok(())
    }

    /// What is written made durable, then listed in the sidecar.
    fn persist(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.file.sync_data()?;
        self.write_sidecar()
    }

    fn persist_soon(&mut self) -> io::Result<()> {
        if self.dirty && self.persisted.elapsed() >= PERSIST_EVERY { self.persist() } else { Ok(()) }
    }

    /// Whole: synced, then the sidecar removed (the file is a cached file from here).
    fn finish(&mut self) -> io::Result<()> {
        self.file.sync_all()?;
        remove_if_present(&self.sidecar)
    }
}

/// The sidecar, written whole or not at all (a new file renamed over the old).
fn write_sidecar(sidecar: &Path, size: u64, filled: &[Range<u64>], validator: Option<&str>) -> io::Result<()> {
    let s = Sidecar { size, filled: filled.iter().map(|r| (r.start, r.end)).collect(), validator: validator.map(str::to_owned) };
    let tmp = sidecar.with_extension("partial-new");
    fs::write(&tmp, serde_json::to_vec(&s).unwrap_or_default())?;
    fs::rename(&tmp, sidecar)
}

// ---- one download ----------------------------------------------------------------------

struct Download {
    file: CachedFile,
    info: StreamInfo,
    feed: Feed,
    stop: Mutex<Option<Stop>>,
    woken: Condvar,
    /// The thread has returned (whole, failed or stopped): a new request starts another.
    ended: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stop {
    /// Not wanted any more, or the app is quitting: what is written stays.
    Quit,
    /// Signed out: the feed fails with these words.
    Fail(String),
}

impl Download {
    fn stopping(&self) -> bool {
        self.stop.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Still fetching, so a request for its file joins it. Not once its feed failed: the thread
    /// fails the feed before it returns (and `ended` is set), and a request in between would
    /// get that failed feed back instead of a fresh download.
    fn joinable(&self) -> bool {
        !self.stopping() && !self.ended.load(Ordering::Acquire) && self.feed.failure().is_none()
    }

    fn set_stop(&self, why: Stop) {
        let mut s = self.stop.lock().unwrap_or_else(|e| e.into_inner());
        if s.is_none() {
            *s = Some(why);
        }
        self.woken.notify_all();
    }

    /// Wait `d` unless stopped first; true when stopped.
    fn pause(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        let mut s = self.stop.lock().unwrap_or_else(|e| e.into_inner());
        while s.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self.woken.wait_timeout(s, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }

    /// Wait for the thread to return (whoever took its handle).
    fn join(&self) {
        let h = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            let _ = h.join();
        }
        while !self.ended.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

// ---- the streamer ----------------------------------------------------------------------

struct Inner {
    client: Option<Client>,
    downloads: HashMap<String, Arc<Download>>,
    /// The keys of the tracks the queue plays now and next (`keep`, and each `request` until the
    /// next `keep`): never evicted, and their downloads go on with no reader.
    kept: HashSet<String>,
    /// The one after those, fetched once nothing else downloads.
    ahead: Option<(StreamInfo, CachedFile)>,
    /// Representations already handed to the player for current and gapless-next tracks.
    requested: HashMap<String, CachedFile>,
    quality: Option<u32>,
    /// The words of the last failed download of each song (for the pill).
    failures: HashMap<String, String>,
    cap: u64,
    /// Goes up when the account changes (`set_client`) and when the cache is cleared (`clear`):
    /// a clear that waited for its downloads sees by it that another session began meanwhile.
    generation: u64,
}

pub struct Streamer {
    cfg: Config,
    inner: Mutex<Inner>,
    /// Stream requests made (every connection, retries and ranges included).
    pub requests: AtomicUsize,
    /// Downloads whose thread is running.
    pub running: AtomicUsize,
    /// The bytes the cache held when last counted (`counted`): what Settings shows.
    known: AtomicU64,
}

impl Streamer {
    pub fn new(cfg: Config) -> Arc<Self> {
        let _ = fs::create_dir_all(&cfg.dir);
        let cap = cfg.cap;
        Arc::new(Streamer {
            cfg,
            inner: Mutex::new(Inner { client: None, downloads: HashMap::new(), kept: HashSet::new(), ahead: None, requested: HashMap::new(), quality: None, failures: HashMap::new(), cap, generation: 0 }),
            requests: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            known: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.cfg.dir
    }

    /// The cache's bytes, just counted on this (worker) thread: kept, and said to `changed`.
    fn counted(&self, bytes: u64) {
        self.known.store(bytes, Ordering::Relaxed);
        if let Some(f) = &self.cfg.changed {
            f(bytes);
        }
    }

    /// The bytes the cache held when last counted: no look at the folder (the UI thread asks).
    pub fn known_used(&self) -> u64 {
        self.known.load(Ordering::Relaxed)
    }

    /// Signed in (or the sign-in renewed), or signed out (`None`): every download stops and its
    /// feed fails, so the engine moves on with words for it.
    pub fn set_client(&self, client: Option<Client>) {
        let mut g = self.lock();
        let same = match (&g.client, &client) {
            (Some(a), Some(b)) => a.same_credentials(b),
            (None, None) => true,
            _ => false,
        };
        g.client = client.map(|mut c| {
            c.set_stream_timeout_ms(self.cfg.read_timeout_ms);
            c
        });
        if !same {
            for d in g.downloads.values() {
                d.set_stop(Stop::Fail(SIGNED_OUT.to_owned()));
            }
            g.ahead = None;
            g.requested.clear();
            g.generation += 1;
        }
    }

    /// A complete original costs no network bandwidth, even when a cap is set.
    fn preferred_file(&self, g: &Inner, info: &StreamInfo) -> CachedFile {
        let original = CachedFile::new(&self.cfg.dir, info, None);
        if let Some(cap) = g.quality
            && !info.bitrate_kbps.is_some_and(|rate| rate > 0 && rate <= cap)
            && !g.downloads.get(&original.key).is_some_and(|d| d.joinable() && d.feed.is_complete())
            && !original.whole()
        {
            CachedFile::new(&self.cfg.dir, info, Some(cap))
        } else {
            original
        }
    }

    fn set_quality(&self, bitrate: Option<u32>) {
        self.lock().quality = bitrate;
    }

    /// The engine's request for a server-only track: the cached file when it is whole, else the
    /// file growing as it downloads (the download started, or the one under way joined).
    pub fn request(self: &Arc<Self>, token: u64, info: &StreamInfo) -> Result<TrackRequest, Refusal> {
        let mut g = self.lock();
        let file = self.preferred_file(&g, info);
        let req = if let Some(d) = g.downloads.get(&file.key).filter(|d| d.joinable()) {
            TrackRequest::growing(token, d.file.path.clone(), d.feed.clone())
        } else if file.whole() {
            g.failures.remove(&info.song_id);
            touch(&file.path);
            TrackRequest::new(token, file.path.clone())
        } else {
            let Some(client) = g.client.clone() else { return Err(Refusal::NoClient) };
            let d = self.start(&mut g, info, file.clone(), client);
            TrackRequest::growing(token, d.file.path.clone(), d.feed.clone())
        };
        g.kept.insert(file.key.clone());
        g.requested.insert(key(info), file);
        Ok(req)
    }

    /// The tracks the queue plays now and next (kept and fetched; the rest stop once the engine
    /// lets go of them), and the one after them, fetched once nothing else downloads.
    pub fn keep(self: &Arc<Self>, now: &[StreamInfo], ahead: Option<StreamInfo>) {
        let mut g = self.lock();
        let ids: HashSet<_> = now.iter().map(key).collect();
        g.requested.retain(|id, _| ids.contains(id));
        g.kept.clear();
        for info in now {
            let file = g.requested.get(&key(info)).cloned().unwrap_or_else(|| self.preferred_file(&g, info));
            g.kept.insert(file.key.clone());
            if let Some(client) = g.client.clone()
                && !g.downloads.get(&file.key).is_some_and(|d| d.joinable())
                && !file.whole()
            {
                self.start(&mut g, info, file, client);
            }
        }
        g.ahead = ahead.filter(|a| !ids.contains(&key(a))).map(|info| {
            let file = self.preferred_file(&g, &info);
            g.kept.insert(file.key.clone());
            (info, file)
        });
        self.start_ahead(&mut g);
    }

    /// The track ahead, when nothing else is being fetched.
    fn start_ahead(self: &Arc<Self>, g: &mut Inner) {
        let idle = g.downloads.values().all(|d| d.ended.load(Ordering::Acquire) || d.stopping());
        if idle && let Some(client) = g.client.clone() && let Some((info, file)) = g.ahead.take()
            && !file.whole()
        {
            self.start(g, &info, file, client);
        }
    }

    fn start(self: &Arc<Self>, g: &mut Inner, info: &StreamInfo, file: CachedFile, mut client: Client) -> Arc<Download> {
        if file.bitrate.is_some() {
            client.set_stream_timeout_ms(subsonic::STREAM_TIMEOUT_MS.max(self.cfg.read_timeout_ms));
        }
        let key = file.key.clone();
        // One writer per file: a download still stopping is waited for by its successor.
        let before = g.downloads.remove(&key);
        if let Some(b) = &before {
            b.set_stop(Stop::Quit);
        }
        g.failures.remove(&info.song_id);
        let d = Arc::new(Download {
            file,
            info: info.clone(),
            feed: Feed::new(),
            stop: Mutex::new(None),
            woken: Condvar::new(),
            ended: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        g.downloads.insert(key, d.clone());
        self.running.fetch_add(1, Ordering::SeqCst);
        let (s, dd) = (self.clone(), d.clone());
        let spawned = std::thread::Builder::new().name("mp-stream".into()).spawn(move || {
            if let Some(b) = before {
                b.join();
            }
            s.download(&dd, &client);
            dd.ended.store(true, Ordering::Release);
            s.running.fetch_sub(1, Ordering::SeqCst);
            s.finished(&dd);
        });
        match spawned {
            Ok(h) => *d.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(h),
            Err(e) => {
                d.feed.fail(format!("the download could not start ({e})"));
                d.ended.store(true, Ordering::Release);
                self.running.fetch_sub(1, Ordering::SeqCst);
            }
        }
        d
    }

    /// A download's thread returned: let it go, start the one ahead, keep to the cap.
    fn finished(self: &Arc<Self>, d: &Arc<Download>) {
        {
            let mut g = self.lock();
            let current = g.downloads.get(&d.file.key).is_none_or(|x| Arc::ptr_eq(x, d));
            if current {
                g.downloads.remove(&d.file.key);
            }
            // The player may already have requested another representation of the song.
            if current && let Some(m) = d.feed.failure()
                && g.requested.get(&key(&d.info)).is_none_or(|f| f.key == d.file.key)
            {
                g.failures.insert(d.info.song_id.clone(), m);
            }
            self.start_ahead(&mut g);
        }
        let bytes = if d.feed.is_complete() { self.evict() } else { self.used() };
        self.counted(bytes);
    }

    /// The songs being fetched now (the bench reports them).
    #[cfg(feature = "bench")]
    pub fn active(&self) -> Vec<String> {
        self.lock().downloads.iter().filter(|(_, d)| !d.ended.load(Ordering::Acquire)).map(|(k, _)| k.clone()).collect()
    }

    /// A whole file for this song is in the cache.
    #[cfg(feature = "bench")]
    pub fn cached(&self, info: &StreamInfo) -> bool {
        self.preferred_file(&self.lock(), info).whole()
    }

    /// Why the last download of this song failed, if it did.
    pub fn failure(&self, song_id: &str) -> Option<String> {
        self.lock().failures.get(song_id).cloned()
    }

    /// The thread of one download: see the module doc.
    fn download(&self, d: &Download, client: &Client) {
        let sidecar = sidecar_path(&self.cfg.dir, &d.file.key);
        let mut disk = match Disk::open(&d.file, &sidecar) {
            Ok(disk) => disk,
            Err(e) => {
                log::warn!("stream cache: cannot write {}: {e}", d.file.path.display());
                d.feed.fail(format!("the stream cache cannot be written ({e})"));
                return;
            }
        };
        if let Some(t) = disk.total {
            d.feed.set_length(Some(t));
            // Stretches not yet confirmed are told once an answer confirms them (`Sink::version`).
            if disk.confirmed {
                for r in &disk.filled {
                    d.feed.fill(r.clone());
                }
            }
        }
        let started = Instant::now();
        let mut cursor = disk.filled.first().filter(|r| r.start == 0).map_or(0, |r| r.end);
        let mut failures = 0usize;
        let mut ranges = 0usize;
        let generated = d.file.bitrate.is_some();
        let mut in_order = generated;
        loop {
            if disk.whole() {
                match disk.finish() {
                    Ok(()) => {
                        d.feed.complete();
                        log::info!("stream: {} whole in {:.1} s ({ranges} ranges asked)", d.file.key, started.elapsed().as_secs_f64());
                    }
                    Err(e) => d.feed.fail(format!("the stream cache cannot be written ({e})")),
                }
                return;
            }
            if let Some(why) = self.stop_reason(d) {
                log::info!("stream: {} stopped ({why:?}) with {} of {:?} bytes on disk", d.file.key, disk.filled.iter().map(|r| r.end - r.start).sum::<u64>(), disk.total);
                let _ = disk.persist();
                if let Stop::Fail(m) = why {
                    d.feed.fail(m);
                }
                return;
            }
            let (from, limit) = match disk.total {
                // A server that ignores ranges: what is missing first, from 0.
                Some(t) if in_order => (gaps(&disk.filled, t).first().map_or(t, |g| g.start), None),
                Some(t) => match next_stretch(&disk.filled, t, d.feed.wanted(), cursor) {
                    Some(r) => (r.start, Some(r.end)),
                    None => (t, None),
                },
                // No length yet: from the start (a range needs the length to be worth asking).
                None => (0, None),
            };
            if from > 0 {
                ranges += 1;
            }
            let validator = disk.validator.clone();
            let mut sink = Sink { s: self, d, disk: &mut disk, from, pos: from, limit, ended: None, io: None, wrote: 0, in_order };
            self.requests.fetch_add(1, Ordering::Relaxed);
            let r = client.stream_quality(&d.info.song_id, d.file.bitrate, from, validator.as_deref(), &mut sink);
            let (pos, ended, io_err, wrote) = (sink.pos, sink.ended.take(), sink.io.take(), sink.wrote);
            in_order |= sink.in_order;
            cursor = pos;
            if wrote > 0 {
                failures = 0;
            }
            if let Some(e) = io_err {
                log::warn!("stream cache: writing {}: {e}", d.file.path.display());
                d.feed.fail(format!("the stream cache cannot be written ({e})"));
                return;
            }
            if let Some(m) = ended.as_ref().and_then(|e| if let Ended::Failed(m) = e { Some(m.clone()) } else { None }) {
                d.file.discard_partial(&sidecar);
                d.feed.fail(m);
                return;
            }
            // An answer with nothing in it, the file not whole: as a failed connection (else a
            // server answering empty would be asked again and again).
            let r = match r {
                Ok(done) if generated && !done.stopped && (pos == 0 || disk.total.is_some_and(|len| len != pos)) =>
                    Err(subsonic::Error::BadResponse("the server returned an incomplete MP3".into())),
                Ok(_) if wrote == 0 && disk.total.is_some() && !disk.whole() => Err(subsonic::Error::Network("the server sent none of the file".into())),
                r => r,
            };
            match r {
                Ok(done) => {
                    if disk.total.is_none() && !done.stopped && ended.is_none() {
                        // Read to its end with no length said: the file is what came.
                        disk.total = Some(pos);
                    }
                    let _ = disk.persist_soon();
                }
                // A fresh encoder run cannot resume bytes already given to the decoder.
                Err(e) if e.is_retryable() && !(generated && wrote > 0) => {
                    if generated {
                        // No body bytes were published. A new encoder session has a new head.
                        disk.total = None;
                        disk.validator = None;
                    }
                    failures += 1;
                    let _ = disk.persist();
                    if failures >= self.cfg.attempts {
                        log::warn!("stream: {} gave up after {failures} failures: {e}", d.file.key);
                        d.feed.fail(words(&e));
                        return;
                    }
                    let wait = self.cfg.backoff.get(failures - 1).or(self.cfg.backoff.last()).copied().unwrap_or(Duration::from_secs(1));
                    log::info!("stream: {} at {pos}: {e}; again in {:.1} s", d.file.key, wait.as_secs_f64());
                    // A stop during the pause is seen at the top of the loop.
                    d.pause(wait);
                }
                Err(e) => {
                    log::warn!("stream: {}: {e}", d.file.key);
                    if disk.filled.is_empty() {
                        // Nothing came: no partial file to go on from.
                        d.file.discard_partial(&sidecar);
                    } else {
                        let _ = disk.persist();
                    }
                    d.feed.fail(words(&e));
                    return;
                }
            }
        }
    }

    /// Why the download should stop now, if it should.
    fn stop_reason(&self, d: &Download) -> Option<Stop> {
        if let Some(s) = d.stop.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(s);
        }
        let kept = self.lock().kept.contains(&d.file.key);
        (!kept && d.feed.readers() == 0).then_some(Stop::Quit)
    }

    /// Every download told to stop (none fails: the app is quitting, or a test is done).
    pub fn shutdown(&self) {
        for d in self.lock().downloads.values() {
            d.set_stop(Stop::Quit);
        }
    }

    /// `shutdown`, and wait for the threads (tests and the bench: nothing left behind).
    #[cfg(any(test, feature = "bench"))]
    pub fn shutdown_and_wait(&self, within: Duration) -> bool {
        self.shutdown();
        let deadline = Instant::now() + within;
        while self.running.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Bytes the cache holds (whole and partial files, sidecars): reads the folder, so only off
    /// the UI thread.
    pub fn used(&self) -> u64 {
        entries(&self.cfg.dir).iter().map(|e| e.bytes).sum()
    }

    pub fn set_cap(&self, cap: u64) {
        self.lock().cap = cap;
        let bytes = self.evict();
        self.counted(bytes);
    }

    /// The cap kept: the least recently played files deleted first, never one kept or being
    /// written. Returns the bytes left.
    pub fn evict(&self) -> u64 {
        let (cap, busy) = {
            let g = self.lock();
            let busy: HashSet<String> = g.kept.iter().cloned().chain(g.downloads.iter().filter(|(_, d)| !d.ended.load(Ordering::Acquire)).map(|(k, _)| k.clone())).collect();
            (g.cap, busy)
        };
        let mut all = entries(&self.cfg.dir);
        let mut total: u64 = all.iter().map(|e| e.bytes).sum();
        if total <= cap {
            return total;
        }
        all.sort_by_key(|e| e.used);
        for e in all {
            if total <= cap {
                break;
            }
            if busy.contains(&e.key) {
                continue;
            }
            for f in &e.files {
                let _ = fs::remove_file(f);
            }
            log::info!("stream cache: {} evicted ({} bytes)", e.key, e.bytes);
            total = total.saturating_sub(e.bytes);
        }
        total
    }

    /// Delete the cache. `all`: everything, every download stopped and failed (sign-out). Else
    /// the tracks kept (playing, next) stay, with their downloads (Settings' Clear).
    ///
    /// Waiting for a stopped download can take up to its read timeout; a new sign-in in that time
    /// starts a new session, whose tracks (kept, or being fetched) are left alone: only what is
    /// none of theirs is deleted. Each song's files are checked and deleted under the lock, so no
    /// download can start on them in between.
    pub fn clear(&self, all: bool) {
        let (keep, mine, ds) = {
            let mut g = self.lock();
            g.generation += 1;
            let keep: HashSet<String> = if all {
                for d in g.downloads.values() {
                    d.set_stop(Stop::Fail(SIGNED_OUT.to_owned()));
                }
                g.ahead = None;
                g.kept.clear();
                g.requested.clear();
                HashSet::new()
            } else {
                g.kept.iter().cloned().chain(g.downloads.iter().filter(|(_, d)| !d.stopping()).map(|(k, _)| k.clone())).collect()
            };
            let ds: Vec<Arc<Download>> = if all { g.downloads.values().cloned().collect() } else { Vec::new() };
            (keep, g.generation, ds)
        };
        // The writers first, or one could write its sidecar again after it was deleted.
        for d in ds {
            d.join();
        }
        for e in entries(&self.cfg.dir) {
            if keep.contains(&e.key) {
                continue;
            }
            let g = self.lock();
            // Since the clear began: another sign-in, another clear, or (Settings' Clear) a track
            // asked for. Its tracks stay.
            let theirs = (g.generation != mine || !all) && (g.kept.contains(&e.key) || g.downloads.get(&e.key).is_some_and(|d| !d.stopping()));
            if theirs {
                continue;
            }
            for f in &e.files {
                let _ = fs::remove_file(f);
            }
            drop(g);
        }
        self.counted(self.used());
    }
}

/// One song's files in the cache.
struct Entry {
    key: String,
    files: Vec<PathBuf>,
    bytes: u64,
    used: SystemTime,
}

fn entries(dir: &Path) -> Vec<Entry> {
    let mut by: HashMap<String, Entry> = HashMap::new();
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let k = key_of(&name).to_owned();
        let used = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let en = by.entry(k.clone()).or_insert_with(|| Entry { key: k, files: Vec::new(), bytes: 0, used: SystemTime::UNIX_EPOCH });
        en.files.push(e.path());
        en.bytes += m.len();
        en.used = en.used.max(used);
    }
    by.into_values().collect()
}

/// How a request's body ended early, by the sink's doing.
#[derive(Debug)]
enum Ended {
    /// The stretch reached what is on disk (or the file's end).
    Done,
    /// A reader waits elsewhere: ask there.
    Jump,
    Stopped,
    /// The file is not the one being written (another length): it changed on the server.
    Failed(String),
}

struct Sink<'a> {
    s: &'a Streamer,
    d: &'a Download,
    disk: &'a mut Disk,
    from: u64,
    pos: u64,
    limit: Option<u64>,
    ended: Option<Ended>,
    io: Option<io::Error>,
    wrote: u64,
    /// The server sends the whole file whatever range is asked: no jumps, in order from 0.
    in_order: bool,
}

/// What an answer says of the file on disk (`Sink::version`).
enum Version {
    /// The same file (or nothing tells otherwise): go on.
    Same,
    /// Another file, and the partial one was emptied for it.
    Restarted,
}

impl Sink<'_> {
    /// Whether this answer is of the file the stretches on disk are of. Stretches kept from an
    /// earlier run are confirmed (and told to the feed) or, when the answer is of another file,
    /// dropped: the partial file is emptied and written afresh. Within a download whose bytes the
    /// feed has had, another file ends it (`Ended::Failed`, set here) as a change of size does.
    fn version(&mut self, head: &StreamHead) -> io::Result<Version> {
        let disk = &mut *self.disk;
        if !disk.confirmed {
            // With a validator, its own word; without, whether the If-Range sent with the range
            // was met.
            let same = match head.validator.as_deref() {
                Some(v) => disk.validator.as_deref() == Some(v),
                None => self.from > 0 && head.start == self.from,
            };
            if same {
                disk.confirmed = true;
                for r in &disk.filled {
                    self.d.feed.fill(r.clone());
                }
                return Ok(Version::Same);
            }
            log::info!("stream: {} changed on the server since its partial file was written: written afresh", self.d.file.key);
            disk.restart(head.total, head.validator.clone())?;
            return Ok(Version::Restarted);
        }
        match (disk.validator.as_deref(), head.validator.as_deref()) {
            (Some(a), Some(b)) if a != b => {
                log::warn!("stream: {} changed on the server while it downloaded", self.d.file.key);
                self.ended = Some(Ended::Failed("the file changed on the server while it downloaded: play it again".to_owned()));
            }
            (None, Some(b)) => {
                disk.validator = Some(b.to_owned());
                disk.dirty = true;
            }
            _ => {}
        }
        Ok(Version::Same)
    }
}

impl StreamSink for Sink<'_> {
    fn start(&mut self, head: &StreamHead) -> ControlFlow<()> {
        match (self.disk.total, head.total) {
            (None, t) => {
                self.disk.total = t;
                // Live MP3s may have a length but no byte-range support. Let the decoder
                // open sequentially rather than waiting for trailing tags.
                self.d.feed.set_length(t.filter(|_| self.d.file.bitrate.is_none()));
            }
            (Some(a), Some(b)) if a != b => {
                log::warn!("stream: {} is {b} bytes on the server now, not {a}", self.d.file.key);
                self.ended = Some(Ended::Failed("the file changed on the server while it downloaded: play it again".to_owned()));
                return ControlFlow::Break(());
            }
            _ => {}
        }
        match self.version(head) {
            Ok(Version::Same) => {}
            Ok(Version::Restarted) => {
                // The new file, from where this body starts: read on (not "in order": the server
                // does ranges, it only had another file).
                if head.start != self.from {
                    self.limit = None;
                }
                self.pos = head.start;
                return ControlFlow::Continue(());
            }
            Err(e) => {
                self.io = Some(e);
                return ControlFlow::Break(());
            }
        }
        if self.ended.is_some() {
            return ControlFlow::Break(());
        }
        if head.start != self.from {
            // The range was not honoured: the body is the file from its start, and so will every
            // other be. Read in order from here on (asking elsewhere would only start it again).
            log::info!("stream: {} asked from {}, the server sends from {}: reading in order", self.d.file.key, self.from, head.start);
            self.limit = None;
            self.in_order = true;
        }
        self.pos = head.start;
        ControlFlow::Continue(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
        if let Err(e) = self.disk.write_at(self.pos, bytes) {
            self.io = Some(e);
            return ControlFlow::Break(());
        }
        let at = self.pos;
        self.pos += bytes.len() as u64;
        self.wrote += bytes.len() as u64;
        // Written to the file (another handle reads it now), then said.
        self.d.feed.fill(at..self.pos);
        if let Err(e) = self.disk.persist_soon() {
            self.io = Some(e);
            return ControlFlow::Break(());
        }
        if self.limit.is_some_and(|l| self.pos >= l) {
            self.ended = Some(Ended::Done);
            return ControlFlow::Break(());
        }
        if self.s.stop_reason(self.d).is_some() {
            self.ended = Some(Ended::Stopped);
            return ControlFlow::Break(());
        }
        if self.disk.total.is_some()
            && !self.in_order
            && let Some(w) = self.d.feed.wanted()
            // Not on disk, so behind `from`, or ahead of what is being read.
            && !(self.pos..=self.pos + JUMP).contains(&w)
        {
            self.ended = Some(Ended::Jump);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

// ---- the app's one streamer --------------------------------------------------------------

struct Hub {
    streamer: Option<Arc<Streamer>>,
    /// Set before `install` (a sign-in that came first): handed over then.
    client: Option<Client>,
    quality: Option<u32>,
}

static HUB: Mutex<Hub> = Mutex::new(Hub { streamer: None, client: None, quality: None });

fn hub() -> MutexGuard<'static, Hub> {
    HUB.lock().unwrap_or_else(|e| e.into_inner())
}

fn streamer() -> Option<Arc<Streamer>> {
    hub().streamer.clone()
}

/// Start the app's streamer on its cache folder with the cap set (main.rs, once). `changed` runs
/// (on a worker thread) with the bytes used when they may have changed.
pub fn install(dir: PathBuf, cap: u64, changed: Arc<dyn Fn(u64) + Send + Sync>) {
    let mut cfg = Config::new(dir, cap);
    cfg.changed = Some(changed);
    let s = Streamer::new(cfg);
    let mut h = hub();
    s.set_client(h.client.take());
    s.set_quality(h.quality);
    h.streamer = Some(s.clone());
    drop(h);
    // A cap lowered while the app was closed, or files left from before; and the first count.
    std::thread::Builder::new()
        .name("mp-stream-cap".into())
        .spawn(move || {
            let bytes = s.evict();
            s.counted(bytes);
        })
        .ok();
}

/// The signed-in client, or `None` when signed out or not connected: called when the app
/// connects, signs in or signs out (the server account's side, `server/mod.rs`). Downloads under way
/// stop when the account changes or goes (their tracks fail with "signed out of the server").
/// Only reads are made with it (`stream`; lyrics use it through `client`).
pub fn set_client(client: Option<Client>) {
    {
        let mut h = hub();
        match &h.streamer {
            Some(s) => s.set_client(client),
            None => h.client = client,
        }
    }
    // Settings > Playback shows the cache once there is a server.
    crate::state::post(refresh_ui);
}

/// Use the selected cap for future streams, including a preference loaded before installation.
pub fn set_quality(bitrate: Option<u32>) {
    let mut h = hub();
    h.quality = bitrate;
    if let Some(s) = &h.streamer {
        s.set_quality(bitrate);
    }
}

/// The client set (`set_client`), for the other reads of a server track (its lyrics).
pub fn client() -> Option<Client> {
    let h = hub();
    match &h.streamer {
        Some(s) => s.lock().client.clone(),
        None => h.client.clone(),
    }
}

/// Delete the whole stream cache, every download stopped (sign-out). Off the calling thread.
pub fn clear_cache() {
    if let Some(s) = streamer() {
        std::thread::Builder::new().name("mp-stream-clear".into()).spawn(move || s.clear(true)).ok();
    }
}

/// Settings' Clear: everything but the tracks playing and next.
pub fn clear_unused() {
    if let Some(s) = streamer() {
        std::thread::Builder::new().name("mp-stream-clear".into()).spawn(move || s.clear(false)).ok();
    }
}

/// The cap changed (Settings > Playback).
pub fn set_cap(cap: u64) {
    if let Some(s) = streamer() {
        std::thread::Builder::new().name("mp-stream-cap".into()).spawn(move || s.set_cap(cap)).ok();
    }
}

/// Bytes the cache held when last counted, on the worker thread that changed it (no look at the
/// folder: the UI thread asks).
pub fn used() -> u64 {
    streamer().map_or(0, |s| s.known_used())
}

/// The app is quitting: every download stops (what is written stays).
pub fn shutdown() {
    if let Some(s) = streamer() {
        s.shutdown();
    }
}

/// The engine's request for a server-only track (`playback`).
pub fn request(token: u64, info: &StreamInfo) -> Result<TrackRequest, Refusal> {
    match streamer() {
        Some(s) => s.request(token, info),
        None => Err(Refusal::NoClient),
    }
}

/// The server tracks the queue plays now and next, and the one after (`playback`).
pub fn keep(now: &[StreamInfo], ahead: Option<StreamInfo>) {
    if let Some(s) = streamer() {
        s.keep(now, ahead);
    }
}

/// Why this song's last download failed, if it did.
pub fn failure(song_id: &str) -> Option<String> {
    streamer().and_then(|s| s.failure(song_id))
}

/// The app's streamer (the bench looks at its counts).
#[cfg(feature = "bench")]
pub fn the_streamer() -> Option<Arc<Streamer>> {
    streamer()
}

// ---- the app's side ------------------------------------------------------------------------

/// How to stream a server-only track, as the library has it (`None`: no longer on the server).
fn info_of(st: &crate::state::State, track: &library::Track) -> Option<StreamInfo> {
    library::remote::stream_info(&st.db, track.id).ok().flatten()
}

/// The engine's request for a server-only track (`playback::request`), or why it cannot be
/// played now, in words for the pill.
pub fn track_request(st: &crate::state::State, token: u64, track: &library::Track) -> Result<TrackRequest, String> {
    let Some(info) = info_of(st, track) else { return Err("it is no longer on the server".to_owned()) };
    request(token, &info).map_err(|r| r.to_string())
}

/// Keep the downloads in step with the queue (`playback::sync_next`, on every change of it and
/// every track start): its current track and its next are kept and fetched; the one after them
/// is fetched once nothing else is. Local tracks cost a look at their path.
pub fn follow_queue(st: &crate::state::State) {
    let (cur, next, after) = {
        let q = st.queue.borrow();
        let items = q.items();
        let after = q.current_index().and_then(|c| match q.repeat {
            crate::queue::Repeat::One => None,
            crate::queue::Repeat::All => items.get((c + 2) % items.len().max(1)),
            crate::queue::Repeat::Off => items.get(c + 2),
        });
        let remote = |i: Option<&crate::queue::Item>| i.filter(|i| i.track.is_remote()).map(|i| i.track.clone());
        (remote(q.current()), remote(q.peek_next()), remote(after))
    };
    let now: Vec<StreamInfo> = [cur, next].into_iter().flatten().filter_map(|t| info_of(st, &t)).collect();
    let ahead = after.and_then(|t| info_of(st, &t));
    keep(&now, ahead);
}

/// Why a server track's download failed, for the pill: its downloader's words, else the
/// engine's message without the file's path.
pub fn failure_words(track: &library::Track, message: &str) -> String {
    track.remote_id.as_deref().and_then(failure).unwrap_or_else(|| message.rsplit(": ").next().unwrap_or(message).to_owned())
}

/// The cache's sizes offered by Settings > Playback, in MB.
pub const CAPS_MB: [u64; 8] = [512, 1024, 2048, 4096, 8192, 16_384, 32_768, 65_536];
pub const DEFAULT_CAP_MB: u64 = 2048;
const MB: u64 = 1 << 20;

/// The size offered `d` steps from `mb` (the nearest offered first, for a hand-edited value).
pub fn cap_step(mb: u64, d: i32) -> u64 {
    let at = CAPS_MB.iter().enumerate().min_by_key(|(_, c)| c.abs_diff(mb)).map_or(2, |(i, _)| i) as i32;
    CAPS_MB[(at + d.signum()).clamp(0, CAPS_MB.len() as i32 - 1) as usize]
}

/// "512 MB", "2 GB".
pub fn cap_text(mb: u64) -> String {
    if mb >= 1024 { format!("{} GB", mb / 1024) } else { format!("{mb} MB") }
}

/// "Empty", "340 MB used", "1.2 GB used".
pub fn used_text(bytes: u64) -> String {
    let mb = bytes as f64 / MB as f64;
    match mb {
        _ if bytes == 0 => "Empty".to_owned(),
        m if m < 1.0 => "Less than 1 MB used".to_owned(),
        m if m < 1024.0 => format!("{m:.0} MB used"),
        m => format!("{:.1} GB used", m / 1024.0),
    }
}

/// Start the app's streamer (main.rs, once, with the settings loaded).
pub fn install_app(st: &std::rc::Rc<crate::state::State>) {
    let mb = st.settings.borrow().stream_cache_mb.max(1);
    install(st.paths.stream(), mb * MB, Arc::new(|bytes| crate::state::post(move |st| show(st, bytes))));
    set_quality(quality_step(st.settings.borrow().stream_bitrate_kbps, 0));
    refresh_ui(st);
}

fn quality_step(bitrate: Option<u32>, d: i32) -> Option<u32> {
    const LEVELS: [Option<u32>; 5] = [Some(96), Some(128), Some(192), Some(320), None];
    let original = LEVELS.len() - 1;
    let at = LEVELS.iter().position(|rate| *rate == bitrate).unwrap_or(original);
    LEVELS[at.saturating_add_signed(d.signum() as isize).min(original)]
}

pub fn step_quality(st: &std::rc::Rc<crate::state::State>, d: i32) {
    let current = st.settings.borrow().stream_bitrate_kbps;
    let rate = quality_step(current, d);
    if rate == current {
        return;
    }
    st.settings.borrow_mut().stream_bitrate_kbps = rate;
    st.save_settings_soon();
    set_quality(rate);
    follow_queue(st);
    refresh_ui(st);
}

/// Whether Settings > Playback > Music from the server is shown (`refresh_ui`), for "Find a
/// setting" (`settingsnav`).
static SETTINGS_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn settings_shown() -> bool {
    SETTINGS_SHOWN.load(Ordering::Relaxed)
}

/// Settings > Playback > Music from the server: shown once there is a server or a cache, with
/// the space used (as last counted: no look at the folder here, on the UI thread) and the cap.
pub fn refresh_ui(st: &std::rc::Rc<crate::state::State>) {
    show(st, used());
}

/// `refresh_ui` with the bytes used, as a worker thread just counted them.
fn show(st: &std::rc::Rc<crate::state::State>, used: u64) {
    use slint::ComponentHandle;
    let Some(app) = st.app.upgrade() else { return };
    let shown = used > 0 || client().is_some() || library::remote::connected(&st.db).unwrap_or(false);
    SETTINGS_SHOWN.store(shown, Ordering::Relaxed);
    let p = app.global::<crate::Player>();
    p.set_stream_shown(shown);
    p.set_stream_used(used_text(used).into());
    p.set_stream_cap(cap_text(st.settings.borrow().stream_cache_mb).into());
    let quality = quality_step(st.settings.borrow().stream_bitrate_kbps, 0);
    p.set_stream_quality(quality.map_or_else(|| "Original".into(), |rate| format!("{rate} kbps").into()));
}

/// Settings > Playback: the cap one step up or down.
pub fn step_cap(st: &std::rc::Rc<crate::state::State>, d: i32) {
    let mb = cap_step(st.settings.borrow().stream_cache_mb, d);
    st.settings.borrow_mut().stream_cache_mb = mb;
    st.save_settings_soon();
    set_cap(mb * MB);
    refresh_ui(st);
}

#[cfg(test)]
mod tests;
