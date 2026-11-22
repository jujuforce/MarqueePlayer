//! `MP_BENCH=streaming`: tracks only the server has, in the real app, against a local mock of a
//! Navidrome server on 127.0.0.1 (`streammock`: nothing leaves the machine). The bench makes its
//! own fixtures with ffmpeg (20 s of FLAC, MP3, Opus, an M4A with `moov` at the end; two FLAC
//! halves of one 8 s signal), writes a small catalog of them into the library as server-only
//! tracks (`library::remote::sync`) and signs the streamer in to the mock (`streaming::set_client`),
//! then plays them through the app's own calls (`bench-streaming.txt`, `st-*.png`):
//!
//! - each format on a link that takes 6 s for the file: the time to first sound, well before the
//!   download ends (Opus and the M4A through a range for their end);
//! - a slow start: the player bar's loading ring, then after a second the pill "Loading from the
//!   server…", both gone once it plays;
//! - a seek far past the download (a range there, playing there before the download is);
//! - a resumed position at start (`play_from`), past what is on disk;
//! - two halves of one album gapless (the second fetched while the first plays, never waited for);
//! - a cached file played again with no request at all, its server lyrics (cached too) synced,
//!   the technical details saying "from the server";
//! - a song gone from the server: the pill says so and the queue moves on;
//! - signed out: a queue of uncached server tracks reached by itself is skipped once and stops at
//!   the second, with one reason; a track chosen is not skipped;
//! - the cap: eviction leaves the track playing;
//! - a skip from a download under way: its thread and connection gone within a moment; Settings >
//!   Playback's card (`st-settings.png`); quit with nothing left running.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use library::remote::{Catalog, RemoteAlbum, RemoteArtist, RemoteSong, StreamInfo, SyncOptions};
use library::{Track, query};
use player_core::PlayState;
use slint::{ComponentHandle, Model, Timer, TimerMode};

use crate::bench::{count_fail, snapshot, write_report};
use crate::state::State;
use crate::server::streammock::{self, Mock};
use crate::server::streaming::{self, Streamer};
use crate::{AppWindow, Lyrics, Nav, Player, playback, views};

struct Song {
    id: &'static str,
    title: &'static str,
    album: usize,
    file: usize,
    suffix: &'static str,
    content_type: &'static str,
    seconds: f64,
}

/// The catalog: (id, title, album, fixture index, suffix, content type, length). Copies of one
/// fixture under other ids are other songs (fetched afresh).
const SONGS: &[Song] = &[
    Song { id: "s-flac", title: "Stream FLAC", album: 0, file: 0, suffix: "flac", content_type: "audio/flac", seconds: 20.0 },
    Song { id: "s-mp3", title: "Stream MP3", album: 0, file: 1, suffix: "mp3", content_type: "audio/mpeg", seconds: 20.0 },
    Song { id: "s-opus", title: "Stream Opus", album: 0, file: 2, suffix: "opus", content_type: "audio/ogg", seconds: 20.0 },
    Song { id: "s-m4a", title: "Stream M4A", album: 0, file: 3, suffix: "m4a", content_type: "audio/mp4", seconds: 20.0 },
    Song { id: "s-a", title: "Half A", album: 1, file: 4, suffix: "flac", content_type: "audio/flac", seconds: 4.025 },
    Song { id: "s-b", title: "Half B", album: 1, file: 5, suffix: "flac", content_type: "audio/flac", seconds: 3.975 },
    Song { id: "s-slow", title: "Slow Start", album: 2, file: 2, suffix: "opus", content_type: "audio/ogg", seconds: 20.0 },
    Song { id: "s-seek", title: "Seek Far", album: 2, file: 0, suffix: "flac", content_type: "audio/flac", seconds: 20.0 },
    // FLAC: its seek bisects, so a resumed position is a range. (An MP3's accurate seek reads
    // every frame up to the place, so there it waits for the download: docs/features/lyrics.md, "Streaming".)
    Song { id: "s-resume", title: "Resumed", album: 2, file: 0, suffix: "flac", content_type: "audio/flac", seconds: 20.0 },
    Song { id: "s-gone", title: "Gone Song", album: 2, file: 1, suffix: "mp3", content_type: "audio/mpeg", seconds: 20.0 },
    Song { id: "s-unc1", title: "Uncached One", album: 2, file: 1, suffix: "mp3", content_type: "audio/mpeg", seconds: 20.0 },
    Song { id: "s-unc2", title: "Uncached Two", album: 2, file: 1, suffix: "mp3", content_type: "audio/mpeg", seconds: 20.0 },
    Song { id: "s-skip", title: "Skipped Away", album: 2, file: 0, suffix: "flac", content_type: "audio/flac", seconds: 20.0 },
    Song { id: "s-held", title: "Removed Loading", album: 2, file: 1, suffix: "mp3", content_type: "audio/mpeg", seconds: 20.0 },
];
const ALBUMS: [&str; 3] = ["Server Streams", "Server Halves", "Server Odds"];

const LYRICS: &str = r#"[{"displayArtist":"Mock","displayTitle":"Stream FLAC","lang":"eng","synced":true,"line":[{"start":0,"value":"First line"},{"start":5000,"value":"Second line"},{"start":10000,"value":"Third line"}]}]"#;

struct Ctx {
    r: String,
    mock: Mock,
    sizes: HashMap<&'static str, u64>,
    tracks: HashMap<&'static str, Track>,
    /// When the task under way began.
    began: Instant,
    /// Scratch for a task (a time, a flag, a count).
    mark: Option<Instant>,
    flag: bool,
    count: usize,
    flashes: Vec<String>,
}

impl Ctx {
    fn line(&mut self, text: String) {
        log::info!("streaming bench: {text}");
        self.r.push_str(&text);
        self.r.push('\n');
    }

    fn check(&mut self, what: &str, ok: bool, detail: String) {
        if !ok {
            count_fail();
        }
        self.line(format!("  [{}] {what}: {detail}", if ok { "ok" } else { "FAIL" }));
    }

    fn track(&self, id: &str) -> Track {
        self.tracks.get(id).cloned().unwrap_or_default()
    }

    fn info(&self, id: &str) -> StreamInfo {
        let s = SONGS.iter().find(|s| s.id == id).unwrap();
        StreamInfo { song_id: id.to_owned(), suffix: Some(s.suffix.into()), content_type: Some(s.content_type.into()), size: self.sizes[id] as i64, ..Default::default() }
    }
}

fn streamer() -> Arc<Streamer> {
    streaming::the_streamer().expect("the app's streamer")
}

/// Playing and heard: started, not loading, the position moving.
fn heard(st: &State) -> bool {
    let s = st.player.status();
    s.state == PlayState::Playing && !playback::loading(st) && s.position > 0.05
}

/// The engine's status is the queue's current track's: right after another track is asked
/// for, the status is still the one before it for a moment (playing, at its position).
fn status_is_current(st: &State) -> bool {
    let uid = st.queue.borrow().current().map(|i| i.uid);
    let token = st.player.status().token;
    uid.is_some() && token.and_then(|t| st.tokens.borrow().get(&t).copied()) == uid
}

fn current_id(st: &State) -> Option<String> {
    st.queue.borrow().current().and_then(|i| i.track.remote_id.clone())
}

fn flash(app: &AppWindow) -> String {
    app.global::<Nav>().get_flash().to_string()
}

/// A task: polled every 20 ms until it returns true, or for at most its limit (a failure).
type Poll = Box<dyn FnMut(&Rc<State>, &AppWindow, &mut Ctx) -> bool>;

struct Task {
    name: String,
    limit: Duration,
    poll: Poll,
}

fn task(name: impl Into<String>, secs: u64, poll: impl FnMut(&Rc<State>, &AppWindow, &mut Ctx) -> bool + 'static) -> Task {
    Task { name: name.into(), limit: Duration::from_secs(secs), poll: Box::new(poll) }
}

/// Run once (then go on).
fn once(name: impl Into<String>, mut f: impl FnMut(&Rc<State>, &AppWindow, &mut Ctx) + 'static) -> Task {
    task(name, 5, move |st, app, c| {
        f(st, app, c);
        true
    })
}

fn wait(ms: u64) -> Task {
    task(format!("wait {ms} ms"), 60, move |_, _, c| c.began.elapsed() >= Duration::from_millis(ms))
}

pub fn run() {
    let mut tasks: Vec<Task> = Vec::new();

    // ---- each format, on a link that takes 6 s for the file ----
    for id in ["s-flac", "s-mp3", "s-opus", "s-m4a"] {
        tasks.push(once(format!("{id}: play"), move |st, _, c| {
            let size = c.sizes[id];
            c.mock.set(|b| b.rate = size / 6);
            c.flag = false;
            c.mark = Some(Instant::now());
            playback::play_tracks(st, vec![c.track(id)], 0);
        }));
        tasks.push(task(format!("{id}: first sound"), 20, move |st, app, c| {
            c.flag |= app.global::<Player>().get_buffering();
            if heard(st) {
                // From the play call (heard: within 20 ms of it, the polling's step).
                c.count = c.mark.map_or(0, |m| m.elapsed().as_millis() as usize);
                return true;
            }
            false
        }));
        tasks.push(task(format!("{id}: whole"), 30, move |_, _, c| {
            if !streamer().cached(&c.info(id)) {
                return false;
            }
            let first = c.count as f64;
            let whole = c.mark.map_or(0.0, |m| m.elapsed().as_secs_f64() * 1000.0);
            let ranges: Vec<u64> = c.mock.streams_of(id).iter().map(|s| s.from).filter(|&f| f > 0).collect();
            let size = c.sizes[id];
            c.line(format!("{id}: {} KB at {} KB/s: first sound {first:.0} ms, whole {whole:.0} ms; loading shown first: {}; ranges from {ranges:?}", size / 1024, size / 6 / 1024, c.flag));
            c.check(&format!("{id} starts well before its download ends"), first < whole * 0.4, format!("{first:.0} of {whole:.0} ms"));
            if id == "s-opus" || id == "s-m4a" {
                c.check(&format!("{id}: its end fetched first, by a range"), !ranges.is_empty(), format!("{ranges:?}"));
            }
            true
        }));
    }

    // ---- a slow start: the ring, then the pill ----
    tasks.push(once("slow: play", |st, _, c| {
        c.mock.set(|b| b.rate = 6 * 1024);
        c.flashes.clear();
        playback::play_tracks(st, vec![c.track("s-slow")], 0);
    }));
    tasks.push(task("slow: the pill after a second", 10, |st, app, c| {
        let f = flash(app);
        if c.began.elapsed() < Duration::from_millis(1300) {
            if c.began.elapsed() > Duration::from_millis(300) && !c.flag {
                c.flag = true;
                let ring = app.global::<Player>().get_buffering();
                let pill = f == playback::LOADING;
                c.check("the loading ring at once, no pill yet", ring && !pill, format!("ring {ring}, pill {f:?}"));
            }
            return false;
        }
        let ring = app.global::<Player>().get_buffering();
        c.check("after a second: \u{201c}Loading from the server\u{2026}\u{201d}", f == playback::LOADING && ring && !heard(st), format!("pill {f:?}, ring {ring}"));
        snapshot(app, "st-loading");
        c.mock.set(|b| b.rate = 0);
        c.flag = false;
        true
    }));
    tasks.push(task("slow: plays on, the pill and ring gone", 15, |st, app, c| {
        if !heard(st) {
            return false;
        }
        let (f, ring) = (flash(app), app.global::<Player>().get_buffering());
        c.check("playing: no pill, no ring", f != playback::LOADING && !ring, format!("pill {f:?}, ring {ring}"));
        true
    }));

    // ---- a server track removed from the queue while it loads, nothing playing ----
    tasks.push(once("removed loading: play", |st, _, c| {
        playback::stop(st);
        c.mock.set(|b| b.rate = 2 * 1024);
        playback::play_tracks(st, vec![c.track("s-held")], 0);
    }));
    tasks.push(wait(400));
    tasks.push(once("removed loading: remove it", |st, _, c| {
        c.flag = playback::loading(st);
        playback::queue_remove(st, 0);
    }));
    tasks.push(wait(300));
    tasks.push(once("removed loading: stopped", |st, app, c| {
        let was = std::mem::take(&mut c.flag);
        let (ring, playing) = (app.global::<Player>().get_buffering(), app.global::<Player>().get_playing());
        c.check("a loading track removed: no longer loading, not shown playing", was && !playback::loading(st) && !ring && !playing, format!("loading before {was}, ring {ring}, playing {playing}"));
        c.mock.set(|b| b.rate = 0);
    }));

    // ---- a seek far past the download ----
    tasks.push(once("seek: play", |st, _, c| {
        let size = c.sizes["s-seek"];
        c.mock.set(|b| b.rate = size / 15);
        playback::play_tracks(st, vec![c.track("s-seek")], 0);
    }));
    tasks.push(task("seek: first sound", 15, |st, _, _| heard(st)));
    tasks.push(once("seek: to 15 s", |st, _, c| {
        c.mark = Some(Instant::now());
        playback::seek(st, 15.0);
    }));
    tasks.push(task("seek: playing at 15 s", 15, |st, _, c| {
        let s = st.player.status();
        if !(heard(st) && s.position > 15.2) {
            return false;
        }
        let took = c.mark.map_or(0.0, |m| m.elapsed().as_secs_f64() * 1000.0);
        let half = c.sizes["s-seek"] / 2;
        let far: Vec<u64> = c.mock.streams_of("s-seek").iter().map(|r| r.from).filter(|&f| f > half).collect();
        let whole = streamer().cached(&c.info("s-seek"));
        c.check("a seek past the download: a range there, playing before the download got there", !far.is_empty() && !whole, format!("{took:.0} ms, ranges from {far:?}, whole {whole}"));
        true
    }));

    // ---- a resumed position, past what is on disk ----
    tasks.push(once("resume: play from 12 s", |st, _, c| {
        let size = c.sizes["s-resume"];
        c.mock.set(|b| b.rate = size / 15);
        st.queue.borrow_mut().replace(vec![c.track("s-resume")], 0);
        let uid = st.queue.borrow().current().map(|i| i.uid).unwrap_or(0);
        st.resume.set(Some((uid, 12.0)));
        c.mark = Some(Instant::now());
        playback::play_current(st);
    }));
    tasks.push(task("resume: playing at 12 s", 15, |st, _, c| {
        let s = st.player.status();
        if !(heard(st) && status_is_current(st) && s.position >= 12.0) {
            return false;
        }
        let took = c.mark.map_or(0.0, |m| m.elapsed().as_secs_f64() * 1000.0);
        let ranges: Vec<u64> = c.mock.streams_of("s-resume").iter().map(|r| r.from).collect();
        c.check("the saved position plays from the server", s.position < 14.0 && ranges.iter().any(|&f| f > 0), format!("at {:.1} s after {took:.0} ms, requests from {ranges:?}", s.position));
        true
    }));

    // ---- two halves gapless ----
    tasks.push(once("gapless: play", |st, _, c| {
        let size = c.sizes["s-a"];
        // The first in about 1.5 s; the second as fast, fetched as soon as it is next.
        c.mock.set(|b| b.rate = size * 2 / 3);
        c.flag = false;
        c.count = 0;
        playback::play_tracks(st, vec![c.track("s-a"), c.track("s-b")], 0);
    }));
    tasks.push(task("gapless: the second follows", 20, |st, app, c| {
        let on_b = current_id(st).as_deref() == Some("s-b");
        if !on_b {
            // Once the first plays, nothing waits again.
            if heard(st) {
                c.count = 1;
            }
            if c.count == 1 && app.global::<Player>().get_buffering() {
                c.flag = true;
            }
            return false;
        }
        if !heard(st) {
            return false;
        }
        let failures = st.play_failures.get();
        let b_asked = c.mock.streams_of("s-b").first().map(|s| s.at);
        let a_asked = c.mock.streams_of("s-a").first().map(|s| s.at);
        let early = matches!((a_asked, b_asked), (Some(a), Some(b)) if b.duration_since(a) < Duration::from_secs(3));
        c.check("two server tracks gapless: the second fetched as it became next, never waited for", !c.flag && failures == 0 && early && playback::next_transition() == player_core::Transition::Gapless, format!("waited {}, failures {failures}, second asked {:?} after the first", c.flag, b_asked.zip(a_asked).map(|(b, a)| b.duration_since(a))));
        true
    }));

    // ---- a cached file again: no request; its lyrics; the details ----
    tasks.push(once("cached: play", |st, _, c| {
        c.mock.set(|b| b.rate = 0);
        c.count = c.mock.seen().len();
        {
            st.settings.borrow_mut().show_details = true;
        }
        crate::metadata::details::sync(st);
        playback::play_tracks(st, vec![c.track("s-flac")], 0);
    }));
    tasks.push(task("cached: plays", 10, |st, _, _| heard(st)));
    tasks.push(wait(800));
    tasks.push(once("cached: no request, lyrics, details", |st, app, c| {
        let asked = c.mock.seen().len() - c.count;
        c.check("a cached file plays with no request at all", asked == 0, format!("{asked} requests"));
        let l = app.global::<Lyrics>();
        let lines = l.get_lines().row_count();
        c.check("the server's synced lyrics show", lines == 3 && l.get_synced(), format!("{lines} lines, synced {}", l.get_synced()));
        let full = app.global::<Player>().get_details_full().to_string();
        c.check("the details say it comes from the server", full.ends_with("from the server") && full.starts_with("FLAC"), format!("{full:?}"));
        crate::views::go(st, crate::Page::NowPlaying);
    }));
    tasks.push(wait(700));
    tasks.push(once("cached: snapshot", |_, app, _| {
        snapshot(app, "st-now-playing");
    }));

    // ---- a song gone from the server ----
    tasks.push(once("gone: play", |st, _, c| {
        c.flashes.clear();
        playback::play_tracks(st, vec![c.track("s-gone"), c.track("s-flac")], 0);
    }));
    tasks.push(task("gone: said, and the queue moves on", 15, |st, app, c| {
        let f = flash(app);
        if !f.is_empty() && c.flashes.last() != Some(&f) {
            c.flashes.push(f);
        }
        if !(current_id(st).as_deref() == Some("s-flac") && heard(st)) {
            return false;
        }
        let said = c.flashes.iter().any(|f| f.contains("Gone Song") && f.contains("no longer on the server"));
        let asked = c.mock.streams_of("s-gone").len();
        c.check("a song gone from the server: said, not retried, the next plays", said && asked == 1, format!("{:?}, {asked} requests", c.flashes));
        true
    }));

    // ---- signed out ----
    tasks.push(once("signed out: a queue reached by itself", |st, _, c| {
        streaming::set_client(None);
        c.flashes.clear();
        c.count = c.mock.seen().len();
        // The cached track plays out from 18 s; then two uncached ones.
        st.queue.borrow_mut().replace(vec![c.track("s-flac"), c.track("s-unc1"), c.track("s-unc2"), c.track("s-flac")], 0);
        let uid = st.queue.borrow().current().map(|i| i.uid).unwrap_or(0);
        st.resume.set(Some((uid, 18.0)));
        playback::play_current(st);
    }));
    tasks.push(task("signed out: skipped once, stopped at the second", 20, |st, app, c| {
        let f = flash(app);
        if !f.is_empty() && c.flashes.last() != Some(&f) {
            c.flashes.push(f);
        }
        let s = st.player.status();
        if !(current_id(st).as_deref() == Some("s-unc2") && s.state == PlayState::Stopped && c.began.elapsed() > Duration::from_secs(1)) {
            return false;
        }
        let asked = c.mock.seen().len() - c.count;
        let reason = c.flashes.iter().filter(|f| f.contains("not signed in to the server")).count();
        c.check("signed out: one skipped, playback stopped at the next, nothing asked", reason >= 1 && asked == 0 && !playback::loading(st), format!("{:?}, {asked} requests", c.flashes));
        // Chosen by the user: said, and not skipped.
        playback::queue_play(st, 1);
        true
    }));
    tasks.push(wait(500));
    tasks.push(once("signed out: a track chosen stays", |st, app, c| {
        let s = st.player.status();
        let f = flash(app);
        c.check("signed out: a track chosen is said and not skipped", current_id(st).as_deref() == Some("s-unc1") && s.state == PlayState::Stopped && f.contains("Uncached One"), format!("{:?}, {f:?}", current_id(st)));
        snapshot(app, "st-signed-out");
        streaming::set_client(Some(c.mock.client()));
    }));

    // ---- the cap ----
    tasks.push(once("cap: play the cached FLAC", |st, _, c| {
        playback::play_tracks(st, vec![c.track("s-flac")], 0);
    }));
    tasks.push(task("cap: plays", 10, |st, _, _| heard(st)));
    tasks.push(once("cap: lowered to one file", |_, _, c| {
        let s = streamer();
        let before = s.used();
        let cap = c.sizes["s-flac"] + 64 * 1024;
        s.set_cap(cap);
        let after = s.used();
        let kept = s.cached(&c.info("s-flac"));
        c.check("the cap: least recently played evicted, the playing track kept", after <= cap && kept && after < before, format!("{} KB to {} KB (cap {} KB), playing kept {kept}", before / 1024, after / 1024, cap / 1024));
        s.set_cap(streaming::DEFAULT_CAP);
    }));

    // ---- a skip from a download under way ----
    tasks.push(once("skip: a slow download", |st, _, c| {
        let size = c.sizes["s-skip"];
        c.mock.set(|b| b.rate = size / 30);
        playback::play_tracks(st, vec![c.track("s-skip")], 0);
    }));
    tasks.push(task("skip: playing", 15, |st, _, c| heard(st) && c.mock.open() >= 1));
    tasks.push(once("skip: to the cached FLAC", |st, _, c| {
        c.mark = Some(Instant::now());
        playback::play_tracks(st, vec![c.track("s-flac")], 0);
    }));
    tasks.push(task("skip: nothing left downloading", 10, |_, _, c| {
        let s = streamer();
        if s.running.load(Ordering::SeqCst) > 0 || c.mock.open() > 0 {
            return false;
        }
        let took = c.mark.map_or(0.0, |m| m.elapsed().as_secs_f64() * 1000.0);
        c.check("a skip stops the download and closes its connection", took < 2000.0, format!("{took:.0} ms"));
        true
    }));

    // ---- Settings > Playback ----
    tasks.push(once("settings: Playback", |st, _, _| {
        streaming::refresh_ui(st);
        views::settings_section(st, crate::settings::nav::PLAYBACK);
    }));
    tasks.push(wait(900));
    tasks.push(once("settings: snapshot", |_, app, c| {
        let p = app.global::<Player>();
        let (shown, used, cap) = (p.get_stream_shown(), p.get_stream_used().to_string(), p.get_stream_cap().to_string());
        c.check("Settings > Playback shows the cache, its size and the cap", shown && used.ends_with("used") && cap == "2 GB", format!("shown {shown}, {used:?}, cap {cap:?}"));
        snapshot(app, "st-settings");
    }));

    // ---- quit ----
    tasks.push(once("quit: nothing left behind", |st, _, c| {
        st.player.stop();
        let s = streamer();
        let done = s.shutdown_and_wait(Duration::from_secs(3));
        c.check("quit: every download ended", done && s.running.load(Ordering::SeqCst) == 0, format!("{} running", s.running.load(Ordering::SeqCst)));
    }));
    tasks.push(task("quit: no connection", 5, |_, _, c| c.mock.open() == 0));

    start(tasks);
}

/// Fixtures, the mock, the catalog and the sign-in; then the tasks, one after another.
fn start(tasks: Vec<Task>) {
    let Some(ctx) = setup::make() else {
        count_fail();
        write_report("streaming", "  [FAIL] setup: ffmpeg is not on PATH, or the catalog could not be written\n");
        let _ = slint::quit_event_loop();
        return;
    };
    let ctx = Rc::new(RefCell::new(ctx));
    let tasks = Rc::new(RefCell::new(tasks.into_iter()));
    let current: Rc<RefCell<Option<Task>>> = Rc::default();
    let timer = Rc::new(Timer::default());
    let t = timer.clone();
    let run_started = Instant::now();
    timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
        crate::state::with(|st| {
            let Some(app) = st.app.upgrade() else { return };
            let mut c = ctx.borrow_mut();
            let mut cur = current.borrow_mut();
            if cur.is_none() {
                match tasks.borrow_mut().next() {
                    Some(next) => {
                        c.began = Instant::now();
                        *cur = Some(next);
                    }
                    None => {
                        t.stop();
                        let n = crate::bench::failed();
                        c.line(format!("{n} failed checks, {:.1} s", run_started.elapsed().as_secs_f64()));
                        write_report("streaming", &c.r);
                        let _ = slint::quit_event_loop();
                        return;
                    }
                }
            }
            let task = cur.as_mut().unwrap();
            if (task.poll)(st, &app, &mut c) {
                *cur = None;
            } else if c.began.elapsed() > task.limit {
                let name = task.name.clone();
                let s = streamer();
                let detail = format!("not within {:?} (downloading {:?}, {} connections open: {:?}; {:?})", task.limit, s.active(), c.mock.open(), c.mock.doing(), st.player.status());
                c.check(&name, false, detail);
                *cur = None;
            }
        });
    });
    std::mem::forget(timer);
}

mod setup {
    use super::*;

    /// Make the fixtures, start the mock with them, write the catalog into the library (a
    /// connection of its own: `sync` writes in transactions), sign the app's streamer in.
    pub fn make() -> Option<Ctx> {
        if !streammock::have_ffmpeg() {
            return None;
        }
        let files = streammock::fixtures(&std::env::temp_dir().join("mp-stream-fixtures"), 20);
        let halves = streammock::fixtures(&std::env::temp_dir().join("mp-stream-fixtures"), 8);
        let mock = Mock::start();
        let mut sizes = HashMap::new();
        for s in SONGS {
            let path = if s.album == 1 { &halves[s.file] } else { &files[s.file] };
            let bytes = std::fs::read(path).ok()?;
            sizes.insert(s.id, bytes.len() as u64);
            if s.id != "s-gone" {
                mock.add(s.id, bytes, s.content_type);
            }
        }
        mock.set_lyrics("s-flac", LYRICS);
        let albums = ALBUMS
            .iter()
            .enumerate()
            .map(|(a, title)| RemoteAlbum {
                id: format!("al-{a}"),
                title: (*title).into(),
                artist: "Mock Server".into(),
                artists: vec![RemoteArtist { name: "Mock Server".into(), ..Default::default() }],
                date: Some("2024".into()),
                songs: SONGS
                    .iter()
                    .filter(|s| s.album == a)
                    .enumerate()
                    .map(|(n, s)| RemoteSong {
                        id: s.id.into(),
                        title: s.title.into(),
                        artist: "Mock Server".into(),
                        track: Some(n as u32 + 1),
                        disc: Some(1),
                        duration: s.seconds,
                        suffix: Some(s.suffix.into()),
                        content_type: Some(s.content_type.into()),
                        size: sizes[s.id] as i64,
                        sample_rate: Some(if s.suffix == "opus" { 48_000 } else { 44_100 }),
                        channels: Some(2),
                        bit_depth: (s.suffix == "flac").then_some(16),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect();
        let catalog = Catalog { albums, complete: true, server_scan: None };
        let tracks = crate::state::with(|st| -> Option<HashMap<&'static str, Track>> {
            let mut conn = library::db::open(&st.paths.database()).ok()?;
            let rep = library::remote::sync(&mut conn, &format!("{} as {}", mock.url, streammock::USER), &catalog, &SyncOptions::default()).ok()?;
            log::info!("streaming bench: catalog of {} songs written", rep.songs);
            let mut t = HashMap::new();
            for s in SONGS {
                let tr = query::track_by_remote(&st.db, s.id).ok()??;
                assert!(tr.is_remote(), "{} is a server-only track", s.id);
                t.insert(s.id, tr);
            }
            Some(t)
        })??;
        streaming::set_client(Some(mock.client()));
        let mut c = Ctx { r: String::new(), mock, sizes, tracks, began: Instant::now(), mark: None, flag: false, count: 0, flashes: Vec::new() };
        c.line(format!("streaming bench: {} server tracks at the mock {}", c.tracks.len(), c.mock.url));
        Some(c)
    }
}
