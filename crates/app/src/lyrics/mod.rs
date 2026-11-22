//! Lyrics on the now playing page. Local first (`library::lyrics`: a `.lrc` beside the
//! track, else its tags), read off the UI thread when the track changes and kept only for
//! the track playing; synced lyrics follow the position on every tick (a binary search).
//!
//! With Settings > Look up lyrics online on, a track without local lyrics is looked up on
//! LRCLIB (`crates/lrclib`): its artist, title, album and length, and nothing else, go to
//! lrclib.net, after the track has played for `WAIT` (skipping through tracks sends
//! nothing), on the lookup thread; answers are cached in the cache folder (`lyrics\`), so
//! each track is asked about once. Only library tracks, and files from outside that have
//! artist and title tags, are looked up. An answer for a track no longer playing is not
//! shown. Off: nothing is sent, and the cache is not read either.
//!
//! A track only the server has (`Track::is_remote`) has no file to read: its lyrics are the
//! server's (`getLyricsBySongId`, synced ones preferred), asked once and kept in `lyrics\server\`
//! in the cache folder (none found is asked again after `SERVER_AGAIN`), then, without any, the
//! online lookup as for a file. A track the server has and a local file also has keeps the local
//! order. The server is asked whatever the online setting: it is the user's own, and the track
//! comes from it anyway.
//!
//! What the Lyrics tab says (`Lyrics.online`): "" (local lyrics, or none and the lookup
//! off), "looking", "found" and "instrumental" (with the source and "Wrong lyrics?"),
//! "not-found", "offline", "failed" and "rejected" (with "Try again").
//!
//! Synced lyrics early or late for a file: the tab's timing row (and G / H) moves them for
//! the track playing, 0.1 s a step (`nudge`), at once for the line lit, the scrolling and a
//! click on a line. The offset is kept per track, by its path, in `lyrics-offsets.json` in
//! the data folder (`Offsets`; nothing in the database, like the lyrics themselves). Copy and
//! Save are `lyrics/save.rs`.

pub(crate) mod save;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use library::Track;
use library::lyrics::{self, Line};
use lrclib::{Answer, Delivery, TrackInfo};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::state::{self, State};
use crate::{LyricLine, Lyrics as LyricsStore};

/// How long a track plays before it is looked up online.
const WAIT: Duration = Duration::from_millis(2500);
/// The server said it has no lyrics for a song: asked again after this.
const SERVER_AGAIN: Duration = Duration::from_secs(14 * 24 * 3600);

struct Online {
    worker: lrclib::Worker,
    enabled: Arc<AtomicBool>,
}

thread_local! {
    /// The path the lyrics on screen (or being read) belong to, and their lines.
    static FOR: RefCell<String> = const { RefCell::new(String::new()) };
    static LINES: RefCell<Vec<Line>> = const { RefCell::new(Vec::new()) };
    static GEN: Cell<u64> = const { Cell::new(0) };
    /// The playing track as it would be looked up, when it may be (see the module doc).
    static LOOKABLE: RefCell<Option<TrackInfo>> = const { RefCell::new(None) };
    /// The track has lyrics of its own.
    static LOCAL: Cell<bool> = const { Cell::new(false) };
    static ONLINE: RefCell<Option<Online>> = const { RefCell::new(None) };
    /// The playing track, and the lyrics the tab shows for it (Copy and Save take them).
    static TRACK: RefCell<Option<Track>> = const { RefCell::new(None) };
    static SHOWN: RefCell<Option<lyrics::Lyrics>> = const { RefCell::new(None) };
    /// The playing track's timing offset, ms (positive: later).
    static OFFSET: Cell<i64> = const { Cell::new(0) };
    static OFFSETS: RefCell<Option<Offsets>> = const { RefCell::new(None) };
}

pub const NONE: &str = "No lyrics for this track. A .lrc file with the same name beside it, or lyrics in its tags, show here.";
const NONE_HINT: &str = "No lyrics for this track. A .lrc file with the same name beside it, or lyrics in its tags, show here; Settings can also look them up online.";
const LOOKING: &str = "Looking up lyrics online\u{2026}";
const NOT_FOUND: &str = "No lyrics for this track, here or on LRCLIB.";
const OFFLINE: &str = "LRCLIB could not be reached to look up lyrics for this track.";
const REJECTED: &str = "You marked the lyrics found online for this track as wrong.";
const INSTRUMENTAL: &str = "Instrumental: LRCLIB lists no words for this track.";

/// Start the lookup thread. Call once, with the settings loaded.
pub fn install(st: &Rc<State>) {
    let enabled = Arc::new(AtomicBool::new(st.settings.borrow().lyrics_online));
    let cache = lrclib::Cache::new(st.paths.lyrics());
    let root = crate::test_override("MP_LRCLIB_API").unwrap_or_else(|| "https://lrclib.net".into());
    let worker = lrclib::Worker::spawn(
        // The client name sent with lookups (LRCLIB asks clients to name themselves).
        lrclib::Client::new(root, crate::about::lyrics_agent()),
        cache,
        enabled.clone(),
        Box::new(|g, d| state::post(move |st| delivered(st, g, d))),
    );
    ONLINE.with(|o| *o.borrow_mut() = Some(Online { worker, enabled }));
    let offsets = Offsets::open(st.paths.data.join(OFFSETS_FILE));
    OFFSETS.with(|o| *o.borrow_mut() = Some(offsets));
}

fn online_on() -> bool {
    ONLINE.with(|o| o.borrow().as_ref().is_some_and(|o| o.enabled.load(Ordering::Acquire)))
}

/// Which track's lyrics are on screen: a result for an earlier one is not shown.
pub fn generation() -> u64 {
    GEN.with(Cell::get)
}

/// The playing track changed (or stopped: `None`).
pub fn track_changed(st: &Rc<State>, track: Option<&Track>) {
    let path = track.map(|t| t.path.clone()).unwrap_or_default();
    if FOR.with(|f| *f.borrow() == path) {
        return;
    }
    FOR.with(|f| *f.borrow_mut() = path.clone());
    let generation = GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    LOCAL.with(|l| l.set(false));
    LOOKABLE.with(|l| *l.borrow_mut() = None);
    ONLINE.with(|o| {
        if let Some(o) = o.borrow().as_ref() {
            o.worker.cancel(generation);
        }
    });
    TRACK.with(|c| *c.borrow_mut() = track.cloned());
    OFFSET.with(|o| o.set(OFFSETS.with(|s| s.borrow().as_ref().map_or(0, |s| s.get(&path)))));
    show(st, None);
    set_state(st, "", "");
    crate::lyrics::save::clear_notice(st);
    let Some(t) = track else { return };
    // The credit as tagged, or the first artist of several joined (`names::service_artist`):
    // LRCLIB's records name an artist as players send it.
    let info = TrackInfo { artist: library::names::service_artist(&t.artist), title: t.title.clone(), album: t.album.clone(), duration: t.duration };
    let outside = t.id < 0;
    // A server-only track: the server's lyrics (or what it said before), never a file's.
    let server = t.is_remote().then(|| t.remote_id.clone()).flatten().map(|id| (id, crate::server::streaming::client(), st.paths.lyrics().join("server")));
    std::thread::Builder::new()
        .name("mp-lyrics".into())
        .spawn(move || {
            let p = std::path::Path::new(&path);
            let l = match &server {
                Some((id, client, dir)) => from_server(dir, client.as_ref().and_then(|c| c.as_subsonic()), id),
                None => lyrics::load(p),
            };
            // A file from outside the library is looked up only when it has artist and
            // title tags of its own (its title may be the file name).
            let lookable = l.is_none()
                && info.lookable()
                && (!outside
                    || library::tags::read(p, false).is_ok_and(|(tags, _)| {
                        tags.artist.is_some_and(|a| !a.trim().is_empty()) && tags.title.is_some_and(|t| !t.trim().is_empty())
                    }));
            state::post(move |st| {
                if GEN.with(Cell::get) != generation {
                    return;
                }
                let local = l.is_some();
                LOCAL.with(|c| c.set(local));
                show(st, l);
                if local {
                    return;
                }
                LOOKABLE.with(|c| *c.borrow_mut() = lookable.then_some(info));
                look_up(st, false);
            });
        })
        .ok();
}

/// Look the playing track up (it has no local lyrics), or say why not.
fn look_up(st: &Rc<State>, retry: bool) {
    let Some(info) = LOOKABLE.with(|c| c.borrow().clone()) else {
        set_state(st, "", NONE);
        return;
    };
    if !online_on() {
        set_state(st, "", NONE_HINT);
        return;
    }
    set_state(st, "looking", LOOKING);
    ONLINE.with(|o| {
        if let Some(o) = o.borrow().as_ref() {
            if retry {
                o.worker.retry(generation(), info);
            } else {
                o.worker.lookup(generation(), info, WAIT);
            }
        }
    });
}

/// An answer from the lookup thread.
fn delivered(st: &Rc<State>, g: u64, d: Delivery) {
    // A track no longer playing, or lyrics of its own: not shown (it is cached anyway).
    if g != generation() || LOCAL.with(Cell::get) || !online_on() {
        return;
    }
    match d {
        Delivery::Answer(Answer::Found { lyrics, .. }) => {
            show(st, Some(lyrics));
            set_state(st, "found", "");
        }
        Delivery::Answer(Answer::Instrumental { .. }) => set_state(st, "instrumental", INSTRUMENTAL),
        Delivery::Answer(Answer::NotFound) => set_state(st, "not-found", NOT_FOUND),
        Delivery::Answer(Answer::Rejected) => set_state(st, "rejected", REJECTED),
        Delivery::Offline(_) => set_state(st, "offline", OFFLINE),
        Delivery::Failed(m) => set_state(st, "failed", &format!("Lyrics could not be looked up: {m}.")),
        Delivery::Disabled => set_state(st, "", NONE_HINT),
    }
}

/// "Wrong lyrics?": drop what was found for the playing track and never look it up
/// again by itself.
pub fn reject(st: &Rc<State>) {
    let Some(info) = LOOKABLE.with(|c| c.borrow().clone()) else { return };
    let sent = ONLINE.with(|o| {
        let o = o.borrow();
        let Some(o) = o.as_ref() else { return false };
        o.worker.reject(info);
        true
    });
    if !sent {
        return;
    }
    show(st, None);
    set_state(st, "rejected", REJECTED);
}

/// "Try again".
pub fn retry(st: &Rc<State>) {
    if LOCAL.with(Cell::get) {
        return;
    }
    show(st, None);
    look_up(st, true);
}

/// Settings > Look up lyrics online changed.
pub fn set_enabled(st: &Rc<State>, on: bool) {
    // Read before every request; an answer arriving after this is not shown (`delivered`).
    ONLINE.with(|o| {
        if let Some(o) = o.borrow().as_ref() {
            o.enabled.store(on, Ordering::Release);
        }
    });
    if LOCAL.with(Cell::get) || FOR.with(|f| f.borrow().is_empty()) {
        return;
    }
    // Online lyrics on screen go with it; turned on, the playing track is looked up.
    show(st, None);
    look_up(st, false);
}

fn set_state(st: &Rc<State>, online: &str, status: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<LyricsStore>();
    s.set_online(online.into());
    s.set_status(status.into());
}

fn show(st: &Rc<State>, l: Option<lyrics::Lyrics>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<LyricsStore>();
    SHOWN.with(|c| *c.borrow_mut() = l.clone().filter(|l| !l.lines.is_empty()));
    let (lines, synced) = l.map_or((Vec::new(), false), |l| (l.lines, l.synced));
    let rows: Vec<LyricLine> = lines
        .iter()
        .map(|l| LyricLine { text: l.text.as_str().into(), time: l.time_ms.map_or(-1.0, |t| t as f32 / 1000.0) })
        .collect();
    s.set_synced(synced);
    s.set_current(-1);
    s.set_lines(ModelRc::new(VecModel::from(rows)));
    s.set_status("".into());
    LINES.with(|v| *v.borrow_mut() = if synced { lines } else { Vec::new() });
    sync_offset(st);
}

/// The position moved: the line being sung (the offset applied).
pub fn tick(st: &Rc<State>, position: f64) {
    let offset = OFFSET.with(Cell::get);
    let Some(i) = LINES.with(|v| {
        let v = v.borrow();
        (!v.is_empty()).then(|| lyrics::current_shifted(&v, (position.max(0.0) * 1000.0) as u64, offset).map_or(-1, |i| i as i32))
    }) else {
        return;
    };
    if let Some(app) = st.app.upgrade() {
        let s = app.global::<LyricsStore>();
        if s.get_current() != i {
            s.set_current(i);
        }
    }
}

/// A synced line was clicked (or Enter on it): play from there (where it shows, the offset
/// applied).
pub fn seek(st: &Rc<State>, index: usize) {
    if let Some(t) = LINES.with(|v| v.borrow().get(index).and_then(|l| l.time_ms)) {
        crate::playback::seek(st, lyrics::shifted(t, OFFSET.with(Cell::get)) as f64 / 1000.0);
    }
}

// ---- the timing offset ---------------------------------------------------------------------

/// The playing track, the lyrics the tab shows for it, and its offset (Copy and Save).
pub fn shown() -> Option<(Track, lyrics::Lyrics, i64)> {
    let t = TRACK.with(|c| c.borrow().clone())?;
    let l = SHOWN.with(|c| c.borrow().clone())?;
    Some((t, l, OFFSET.with(Cell::get)))
}

fn synced_shown() -> bool {
    LINES.with(|v| !v.borrow().is_empty())
}

/// Earlier (`steps` < 0) or later, a step (0.1 s) each; `say`: in the pill too (a key, when
/// the tab may not be in view).
pub fn nudge(st: &Rc<State>, steps: i32, say: bool) {
    if !synced_shown() {
        if say {
            crate::views::flash(st, if SHOWN.with(|c| c.borrow().is_some()) { "These lyrics have no time stamps to move" } else { "No synced lyrics for this track" });
        }
        return;
    }
    let now = lyrics::nudge(OFFSET.with(Cell::get), i64::from(steps) * lyrics::OFFSET_STEP_MS);
    set_offset(st, now);
    if say {
        crate::views::flash(st, &format!("Lyrics timing {}", lyrics::offset_text(now)));
    }
}

/// The playing track's offset, kept for it and applied at once.
pub fn set_offset(st: &Rc<State>, ms: i64) {
    OFFSET.with(|o| o.set(ms));
    let path = FOR.with(|f| f.borrow().clone());
    if !path.is_empty() {
        OFFSETS.with(|o| {
            if let Some(o) = o.borrow_mut().as_mut() {
                let _ = o.set(&path, ms);
            }
        });
    }
    sync_offset(st);
    tick(st, st.player.status().position);
}

fn sync_offset(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<LyricsStore>();
    let ms = OFFSET.with(Cell::get);
    s.set_offset(ms.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
    s.set_offset_text(lyrics::offset_text(ms).into());
}

/// Lyrics read again after a save (`lyrics/save.rs`): shown as the track's own now, and with
/// `reset`, the offset back to 0 (it is in the file's time stamps).
pub fn reloaded(st: &Rc<State>, generation: u64, l: lyrics::Lyrics, reset: bool) {
    if generation != self::generation() {
        return;
    }
    LOCAL.with(|c| c.set(true));
    show(st, Some(l));
    set_state(st, "", "");
    if reset {
        set_offset(st, 0);
    }
}

/// The bench: the tab showing `l` for `track` (a server's track the run has no server for).
#[cfg(feature = "bench")]
pub fn bench_pretend(st: &Rc<State>, track: Track, l: lyrics::Lyrics) {
    TRACK.with(|c| *c.borrow_mut() = Some(track));
    show(st, Some(l));
}

/// Where the offsets are kept, in the data folder.
const OFFSETS_FILE: &str = "lyrics-offsets.json";

/// Each track's timing offset by its path (none: 0), as `lyrics-offsets.json` keeps it: a
/// JSON object of paths and milliseconds. Read once at start; written on a thread of its own
/// after every change (the last change written last), replacing the file whole.
struct Offsets {
    file: PathBuf,
    map: BTreeMap<String, i64>,
}

/// The writes of `lyrics-offsets.json` in turn, and the newest one written.
static OFFSETS_WRITE: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
static OFFSETS_SEQ: AtomicU64 = AtomicU64::new(0);

impl Offsets {
    fn open(file: PathBuf) -> Self {
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<BTreeMap<String, i64>>(&b).ok()).unwrap_or_default();
        let map = map.into_iter().filter(|(_, ms)| *ms != 0).map(|(p, ms)| (p, ms.clamp(-lyrics::OFFSET_MAX_MS, lyrics::OFFSET_MAX_MS))).collect();
        Offsets { file, map }
    }

    fn get(&self, path: &str) -> i64 {
        self.map.get(path).copied().unwrap_or(0)
    }

    /// The write, for a test to wait on (the app does not wait).
    fn set(&mut self, path: &str, ms: i64) -> Option<std::thread::JoinHandle<()>> {
        let changed = if ms == 0 { self.map.remove(path).is_some() } else { self.map.insert(path.to_owned(), ms) != Some(ms) };
        if !changed {
            return None;
        }
        let bytes = serde_json::to_vec_pretty(&self.map).unwrap_or_default();
        let file = self.file.clone();
        let seq = OFFSETS_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        let spawned = std::thread::Builder::new().name("mp-lyrics-offsets".into()).spawn(move || {
            let mut newest = OFFSETS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
            if *newest > seq {
                return;
            }
            *newest = seq;
            if let Err(e) = write_whole(&file, &bytes) {
                log::warn!("lyrics offsets: cannot write {}: {e}", file.display());
            }
        });
        spawned.inspect_err(|e| log::warn!("lyrics offsets: no thread to write them: {e}")).ok()
    }
}

/// Numbers each write's temp file, so writers racing for one file (the server's lyrics of one
/// song, from two `mp-lyrics` threads) never share one.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A small file replaced whole: written beside it under a name of its own, then moved over it;
/// if either fails, the temp file is removed.
fn write_whole(file: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = file.with_extension(format!("json.{}.{seq}.new", std::process::id()));
    let r = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, file));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

// ---- the server's lyrics ---------------------------------------------------------------

/// The server's answer for one song, as kept: its lines (none: it has none), and when it was
/// asked.
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    asked: u64,
    synced: bool,
    lines: Option<Vec<(Option<u64>, String)>>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The lyrics of a server-only song: as kept when they were found (or found missing lately),
/// else asked of the server (no client: what was kept, if anything). Network and server errors
/// are not kept: asked again next time.
fn from_server(dir: &std::path::Path, client: Option<&subsonic::Client>, song_id: &str) -> Option<lyrics::Lyrics> {
    let safe: String = song_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let file = dir.join(format!("{safe}.json"));
    let kept: Option<Kept> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let as_lyrics = |k: &Kept| k.lines.as_ref().map(|l| lyrics::Lyrics { synced: k.synced, lines: l.iter().map(|(t, s)| Line { time_ms: *t, text: s.clone() }).collect(), source: "server" });
    if let Some(k) = &kept
        && (k.lines.is_some() || now_secs().saturating_sub(k.asked) < SERVER_AGAIN.as_secs())
    {
        return as_lyrics(k);
    }
    let Some(client) = client else { return kept.as_ref().and_then(as_lyrics) };
    match client.lyrics_by_song_id(song_id) {
        Ok(list) => {
            let l = pick_server(&list);
            let k = Kept { asked: now_secs(), synced: l.as_ref().is_some_and(|l| l.synced), lines: l.map(|l| l.lines.into_iter().map(|x| (x.time_ms, x.text)).collect()) };
            if let Err(e) = write_whole(&file, &serde_json::to_vec(&k).unwrap_or_default()) {
                log::info!("lyrics from the server: cannot keep them in {}: {e}", file.display());
            }
            as_lyrics(&k)
        }
        Err(e) => {
            log::info!("lyrics from the server: {e}");
            kept.as_ref().and_then(as_lyrics)
        }
    }
}

/// The largest server offset taken, either way (a day), as for a file's `[offset:]`.
const SERVER_OFFSET_MAX_MS: i64 = 86_400_000;

/// The entry to show of the server's: synced lyrics with lines first, else the first with any;
/// the offset applied as LRC's is (a positive one shows the lines sooner), within a day either
/// way; a line timed before the start shows at 0.
fn pick_server(list: &[subsonic::StructuredLyrics]) -> Option<lyrics::Lyrics> {
    let has = |l: &&subsonic::StructuredLyrics| l.line.iter().any(|x| !x.value.trim().is_empty());
    let timed = |l: &subsonic::StructuredLyrics| l.synced && l.line.iter().all(|x| x.start.is_some());
    let e = list.iter().filter(has).find(|l| timed(l)).or_else(|| list.iter().find(has))?;
    let synced = timed(e);
    let offset = e.offset.unwrap_or(0).clamp(-SERVER_OFFSET_MAX_MS, SERVER_OFFSET_MAX_MS);
    let lines = e.line.iter().map(|x| Line { time_ms: if synced { x.start.map(|t| t.saturating_sub(offset).max(0) as u64) } else { None }, text: x.value.clone() }).collect();
    Some(lyrics::Lyrics { synced, lines, source: "server" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(synced: bool, offset: Option<i64>, lines: &[(Option<i64>, &str)]) -> subsonic::StructuredLyrics {
        subsonic::StructuredLyrics {
            synced,
            offset,
            line: lines.iter().map(|(t, v)| subsonic::LyricLine { start: *t, value: (*v).to_owned() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_servers_synced_lyrics_come_first_with_their_offset() {
        let plain = entry(false, None, &[(None, "La"), (None, "Di")]);
        let synced = entry(true, Some(100), &[(Some(0), "La"), (Some(2500), "Di")]);
        let l = pick_server(&[plain.clone(), synced]).unwrap();
        assert!(l.synced);
        assert_eq!(l.lines.iter().map(|l| l.time_ms).collect::<Vec<_>>(), vec![Some(0), Some(2400)]);
        let l = pick_server(&[plain]).unwrap();
        assert!(!l.synced && l.lines[0].time_ms.is_none());
        assert!(pick_server(&[entry(true, None, &[(Some(0), " ")])]).is_none(), "nothing to show");
        assert!(pick_server(&[]).is_none());
    }

    #[test]
    fn racing_writes_of_the_servers_lyrics_each_use_their_own_temp_file() {
        let dir = std::env::temp_dir().join(format!("mp-lyrics-server-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names = |dir: &std::path::Path| {
            let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        // Writers for the same song at once: the file is always one whole answer, and no temp
        // file is left (a rename the system refuses removes its own).
        let file = dir.join("s1.json");
        let writers: Vec<_> = (0..6)
            .map(|w| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for i in 0..40 {
                        let lines = (0..200).map(|n| (Some(n * 10), format!("writer {w} pass {i} line {n}"))).collect();
                        let k = Kept { asked: now_secs(), synced: true, lines: Some(lines) };
                        let _ = write_whole(&file, &serde_json::to_vec(&k).unwrap());
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let k: Kept = serde_json::from_slice(&std::fs::read(&file).unwrap()).expect("one whole answer");
        assert_eq!(k.lines.unwrap().len(), 200);
        assert_eq!(names(&dir), vec!["s1.json"]);
        // A write that cannot be moved into place (a folder is in the way) leaves nothing behind.
        let blocked = dir.join("s2.json");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(write_whole(&blocked, b"{}").is_err());
        assert_eq!(names(&dir), vec!["s1.json", "s2.json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extreme_server_times_and_offsets_never_overflow() {
        let times = |offset, starts: &[i64]| {
            let lines: Vec<_> = starts.iter().map(|t| (Some(*t), "La")).collect();
            pick_server(&[entry(true, offset, &lines)]).unwrap().lines.iter().map(|l| l.time_ms).collect::<Vec<_>>()
        };
        assert_eq!(times(Some(-1), &[i64::MAX]), vec![Some(i64::MAX as u64)], "the latest time stays the latest");
        assert_eq!(times(Some(1), &[i64::MIN]), vec![Some(0)], "the earliest stays at 0");
        assert_eq!(times(Some(i64::MIN), &[0]), vec![Some(SERVER_OFFSET_MAX_MS as u64)], "an offset within a day");
        assert_eq!(times(Some(i64::MAX), &[1000, i64::MAX]), vec![Some(0), Some((i64::MAX - SERVER_OFFSET_MAX_MS) as u64)]);
    }

    #[test]
    fn offsets_are_kept_per_track_and_a_zero_is_forgotten() {
        let dir = std::env::temp_dir().join(format!("mp-lyrics-offsets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join(OFFSETS_FILE);
        let mut o = Offsets::open(file.clone());
        assert_eq!(o.get("M:/a/01.flac"), 0, "nothing kept yet");
        o.set("M:/a/01.flac", 300).unwrap().join().unwrap();
        o.set("navidrome:s/abc", -1_200).unwrap().join().unwrap();
        assert!(o.set("navidrome:s/abc", -1_200).is_none(), "no change, no write");
        // A restart reads them back.
        let o2 = Offsets::open(file.clone());
        assert_eq!((o2.get("M:/a/01.flac"), o2.get("navidrome:s/abc"), o2.get("M:/a/02.flac")), (300, -1_200, 0));
        // Back to 0: gone from the file.
        o.set("M:/a/01.flac", 0).unwrap().join().unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("01.flac") && text.contains("navidrome:s/abc"), "{text}");
        // A damaged file or wild values: read as nothing, or within bounds.
        std::fs::write(&file, r#"{"x": 99999999, "y": 0}"#).unwrap();
        let o3 = Offsets::open(file.clone());
        assert_eq!((o3.get("x"), o3.map.len()), (lyrics::OFFSET_MAX_MS, 1));
        std::fs::write(&file, "{ not json").unwrap();
        assert!(Offsets::open(file).map.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_servers_answer_is_kept_and_none_is_asked_again_later() {
        let dir = std::env::temp_dir().join(format!("mp-lyrics-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Kept lyrics are used with no client at all.
        let k = Kept { asked: now_secs(), synced: true, lines: Some(vec![(Some(1000), "Hello".into())]) };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s1.json"), serde_json::to_vec(&k).unwrap()).unwrap();
        let l = from_server(&dir, None, "s1").unwrap();
        assert!(l.synced && l.lines[0].text == "Hello");
        // "None" lately: not asked again (no client needed to say so).
        let none = Kept { asked: now_secs(), synced: false, lines: None };
        std::fs::write(dir.join("s2.json"), serde_json::to_vec(&none).unwrap()).unwrap();
        assert!(from_server(&dir, None, "s2").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
