//! What the scrobbling services hear of the plays (Last.fm and ListenBrainz), and Settings >
//! Connections > Last.fm.
//!
//! One decision for both services, so they cannot drift apart: when a play counts (`counts`:
//! the rule both document; not the app's own play count, which stays as it is in
//! `playback.rs`), that "now playing" goes as a track starts (`track_started`), and whether this
//! app tells a service of a track at all (`allowed`: Settings > Library > Server's switch). What
//! each service is sent is built from the same track and start time (`Play`). Each service's
//! network, queue and credentials are its own, on its own thread (`lastfm::Scrobbler` here,
//! `listenbrainz::Submitter` through `listenbrainz.rs`), so one failing or slow never delays the
//! other; either, both or neither may be connected.
//!
//! `MP_LASTFM_API=<url>` points the scrobbler at another API root (a local mock), for
//! development only.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use lastfm::{Scrobble, Scrobbler, State as LfState, Status};
use library::query::Track;
use slint::ComponentHandle;

use crate::state::{self, State};
use crate::LastFm;

thread_local! {
    static SCROBBLER: RefCell<Option<Scrobbler>> = const { RefCell::new(None) };
    static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
}

/// The API secret and session key in `lastfm.json` are sealed as `navidrome.json`'s token is (`platform::secret`).
fn sealer() -> lastfm::Sealer {
    use crate::platform::secret;
    lastfm::Sealer { scheme: secret::SCHEME, seal: secret::seal, open: secret::open, write: secret::write_private }
}

pub fn install(st: &Rc<State>) {
    let mut opts = lastfm::Options::new(&st.paths.data);
    opts.agent = crate::about::agent();
    opts.sealer = sealer();
    if let Some(root) = crate::test_override("MP_LASTFM_API") {
        opts.root = root;
    }
    // The key and secret show in their fields (never in the log).
    let (key, secret) = saved_fields(&st.paths.data, &sealer());
    if let Some(app) = st.app.upgrade() {
        let l = app.global::<LastFm>();
        l.set_api_key(key.into());
        l.set_api_secret(secret.into());
    }
    let s = Scrobbler::start(
        opts,
        Box::new(|path| library::tags::musicbrainz_track_id(std::path::Path::new(path))),
        Box::new(|status| state::post(move |st| on_status(st, status))),
        Box::new(|url| {
            // Only ever Last.fm's own authorisation page; never from a bench run.
            if url.starts_with(lastfm::api::AUTH_PAGE) && !crate::bench_run() {
                open_url(url);
            }
        }),
    );
    SCROBBLER.with(|c| *c.borrow_mut() = Some(s));
}

/// What the API key and shared secret fields show at start: `lastfm.json`'s, empty once
/// disconnected (the file is gone).
fn saved_fields(data: &std::path::Path, sealer: &lastfm::Sealer) -> (String, String) {
    let c = lastfm::Credentials::read(&data.join("lastfm.json"), sealer).0;
    (c.api_key, c.api_secret)
}

fn with(f: impl FnOnce(&Scrobbler)) {
    SCROBBLER.with(|c| {
        if let Some(s) = c.borrow().as_ref() {
            f(s);
        }
    });
}

/// Last.fm is set up: plays are queued (sent now, or once reconnected or the key fixed).
fn active() -> bool {
    STATUS.with(|s| s.borrow().as_ref().is_some_and(|s| s.accepting))
}

pub fn on_status(st: &Rc<State>, status: Status) {
    STATUS.with(|s| *s.borrow_mut() = Some(status.clone()));
    show(st, &status);
}

/// What Settings shows for a status (also driven directly by the bench).
pub fn show(st: &Rc<State>, status: &Status) {
    let Some(app) = st.app.upgrade() else { return };
    let l = app.global::<LastFm>();
    let (text, connected, authorising, user) = describe(status);
    l.set_status(text.into());
    l.set_connected(connected);
    l.set_authorising(authorising);
    l.set_user(user.into());
    l.set_attention(needs_attention(status));
}

/// Scrobbling stopped until the user acts: the session is no longer accepted, or the key
/// or secret was refused (marked on Settings' section list).
pub fn needs_attention(s: &Status) -> bool {
    matches!(s.state, LfState::Reconnect | LfState::Error(_))
}

/// (status line, connected, waiting for the browser, user name)
pub fn describe(s: &Status) -> (String, bool, bool, String) {
    let queued = match s.queued {
        0 => String::new(),
        1 => " 1 scrobble waiting to be sent.".into(),
        n => format!(" {n} scrobbles waiting to be sent."),
    };
    let note = if s.note.is_empty() { String::new() } else { format!(" {}.", s.note.trim_end_matches('.')) };
    match &s.state {
        LfState::NoCredentials => (format!("Enter your API key and shared secret, then Connect.{queued}"), false, false, String::new()),
        LfState::Disconnected => (format!("Not connected.{queued}"), false, false, String::new()),
        LfState::Authorising { .. } => (
            "Allow Marquee Player on the Last.fm page that opened in your browser; this connects as soon as you do.".into(),
            false,
            true,
            String::new(),
        ),
        LfState::Connected { user } => (format!("Scrobbling as {user}.{note}{queued}"), true, false, user.clone()),
        LfState::Reconnect => (format!("Last.fm no longer accepts this connection. Connect again to keep scrobbling.{queued}"), false, false, String::new()),
        LfState::Error(e) => (format!("{e}{queued}"), false, false, String::new()),
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The scrobbling rule, Last.fm's and ListenBrainz's alike: a track longer than 30 s counts once
/// it has played half its length or four minutes, whichever comes first (ListenBrainz: "half the
/// track or 4 minutes of the track, whichever is lower"; it gives no minimum length, and the
/// same 30 s keeps the two services' plays the same).
pub fn counts(duration: f64, listened: f64) -> bool {
    duration > 30.0 && listened >= (duration / 2.0).min(240.0)
}

/// One play, as each service will get it (the same track, the same start time), until it counts.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub lastfm: Scrobble,
    pub listenbrainz: listenbrainz::Listen,
}

/// A play as Last.fm takes it. The artist is the credit as tagged ("A feat. B": Last.fm
/// documents `artist` only as "the artist name", asks for metadata from tags, never
/// guessed, and lists featured credits so); a credit we joined from several values
/// ("A; B") is no artist Last.fm knows, so the first of them (`names::service_artist`). The
/// album artist the same way ("Various Artists" for a compilation).
pub fn scrobble_of(t: &Track, started: i64) -> Scrobble {
    Scrobble {
        artist: library::names::service_artist(&t.artist),
        track: t.title.clone(),
        album: t.album.clone(),
        album_artist: library::names::service_artist(&t.album_artist),
        duration: t.duration.round().max(0.0) as u32,
        track_number: t.track_no,
        mbid: None,
        timestamp: started,
        // The file to read its recording id from; a track only the server has has none (the
        // play goes without the id, which Last.fm takes as optional).
        path: (!t.is_remote()).then(|| t.path.clone()),
    }
}

/// Whether the scrobbling services hear of track `t` from this app (Settings > Library > Server,
/// "Scrobble server tracks from this app"; Last.fm and ListenBrainz alike). Off, they do not for
/// a track played from the server, nor, while plays are reported to the server, for a file the
/// server also has: the server hears of that play too, and one that scrobbles itself (to
/// Last.fm or ListenBrainz) would send it again. A track the server does not have is always
/// scrobbled.
pub fn allowed(s: &crate::settings::Settings, t: &Track, db: &rusqlite::Connection) -> bool {
    s.server_lastfm || !(t.is_remote() || (t.remote_id.is_some() && crate::server::write::reports_plays(s, db).unwrap_or(s.server_plays)))
}

/// A track started: "now playing" to each service connected, and the play that may count
/// (none when `allowed` is false). `db`: the library, for the MusicBrainz ids the server gave
/// a track only it has (a file's are read from its tags on ListenBrainz's thread).
pub fn track_started(t: &Track, allowed: bool, db: Option<&rusqlite::Connection>) -> Option<Play> {
    if !allowed || t.artist.trim().is_empty() || t.title.trim().is_empty() {
        return None;
    }
    let started = now();
    let play = Play { lastfm: scrobble_of(t, started), listenbrainz: crate::connections::listens::listen_of(t, started, db) };
    if active() {
        with(|c| c.now_playing(play.lastfm.clone()));
    }
    crate::connections::listens::now_playing(&play.listenbrainz);
    Some(play)
}

/// The play reached the rule (`counts`): to each service that takes plays now.
pub fn scrobble(p: Play) {
    if active() {
        with(|c| c.scrobble(p.lastfm));
    }
    crate::connections::listens::submit(p.listenbrainz);
}

pub fn connect(key: &str, secret: &str) {
    with(|c| {
        c.set_credentials(key, secret);
        c.connect();
    });
}

pub fn cancel() {
    with(Scrobbler::cancel);
}

/// Disconnect (or, not connected, "Remove key and secret"): the key and secret leave their
/// fields, no play is taken from now on, and the scrobbler forgets the account and drops the plays
/// waiting (`lastfm.json` and `scrobbles.json` deleted).
pub fn disconnect(st: &Rc<State>) {
    if let Some(app) = st.app.upgrade() {
        let l = app.global::<LastFm>();
        l.set_api_key("".into());
        l.set_api_secret("".into());
    }
    STATUS.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.accepting = false;
        }
    });
    with(Scrobbler::disconnect);
}

pub fn retry() {
    with(Scrobbler::retry);
}

fn open_url(url: &str) {
    if !crate::platform::shell::open_url(url) {
        log::warn!("could not open the browser for Last.fm's authorisation page");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let s = |state, queued, note: &str| Status { state, queued, note: note.into(), accepting: true };
        assert_eq!(describe(&s(LfState::Connected { user: "alice".into() }, 0, "Sent 3 scrobbles")).0, "Scrobbling as alice. Sent 3 scrobbles.");
        assert_eq!(describe(&s(LfState::Connected { user: "alice".into() }, 2, "")).0, "Scrobbling as alice. 2 scrobbles waiting to be sent.");
        assert!(describe(&s(LfState::Authorising { url: String::new() }, 0, "")).2);
        assert!(describe(&s(LfState::Reconnect, 1, "")).0.contains("Connect again"));
    }

    #[test]
    fn a_session_to_reconnect_needs_attention() {
        let s = |state| Status { state, queued: 0, note: String::new(), accepting: true };
        assert!(needs_attention(&s(LfState::Reconnect)));
        assert!(needs_attention(&s(LfState::Error("Last.fm refused the API key".into()))));
        assert!(!needs_attention(&s(LfState::Connected { user: "alice".into() })));
        assert!(!needs_attention(&s(LfState::NoCredentials)));
        assert!(!needs_attention(&s(LfState::Disconnected)));
    }

    /// A Last.fm stand-in on 127.0.0.1 that takes every call (no request leaves the machine):
    /// each request's form, kept.
    type Forms = std::sync::Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>>;

    fn lastfm_mock() -> (String, Forms) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/2.0/", listener.local_addr().unwrap().port());
        let log: Forms = Default::default();
        let l = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let (mut first, mut len) = (String::new(), 0usize);
                let _ = r.read_line(&mut first);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                let _ = r.read_exact(&mut body);
                let decode = |s: &str| -> String {
                    let b = s.replace('+', " ");
                    let b = b.as_bytes();
                    let (mut out, mut i) = (Vec::new(), 0);
                    while i < b.len() {
                        if b[i] == b'%' && i + 2 < b.len() {
                            out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("3f"), 16).unwrap_or(b'?'));
                            i += 3;
                        } else {
                            out.push(b[i]);
                            i += 1;
                        }
                    }
                    String::from_utf8_lossy(&out).into_owned()
                };
                let form: std::collections::BTreeMap<String, String> =
                    String::from_utf8_lossy(&body).split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect();
                let answer = match form.get("method").map(String::as_str) {
                    Some("track.scrobble") => {
                        let n = form.keys().filter(|k| k.starts_with("artist[")).count();
                        let one = r##"{"track":{"#text":"t"},"ignoredMessage":{"code":"0","#text":""}}"##;
                        let list = if n == 1 { one.to_owned() } else { format!("[{}]", vec![one; n].join(",")) };
                        format!(r#"{{"scrobbles":{{"@attr":{{"accepted":{n},"ignored":0}},"scrobble":{list}}}}}"#)
                    }
                    _ => r#"{"nowplaying":{}}"#.to_owned(),
                };
                l.lock().unwrap().push(form);
                let _ = conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).as_bytes());
            }
        });
        (url, log)
    }

    use std::time::{Duration, Instant};

    use listenbrainz::mock::{Mock, Trouble};

    const LB_TOKEN: &str = "6f1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

    /// A Last.fm scrobbler connected (a session in `lastfm.json`) at `url`, in use as the app's.
    fn use_lastfm(dir: &std::path::Path, url: &str) {
        lastfm::Credentials { api_key: "testkey0123456789".into(), api_secret: "testsecret".into(), session_key: "sk-1".into(), username: "alice".into() }.save(&dir.join("lastfm.json"), &lastfm::Sealer::PLAIN);
        let (tx, rx) = std::sync::mpsc::channel::<Status>();
        let opts = lastfm::Options { root: url.into(), retry_min: Duration::from_millis(100), ..lastfm::Options::new(dir) };
        let tx = std::sync::Mutex::new(tx);
        let s = Scrobbler::start(opts, Box::new(|_| None), Box::new(move |st| drop(tx.lock().unwrap().send(st))), Box::new(|_| {}));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let st = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("connected");
            if matches!(st.state, LfState::Connected { .. }) {
                STATUS.with(|c| *c.borrow_mut() = Some(st));
                break;
            }
        }
        SCROBBLER.with(|c| *c.borrow_mut() = Some(s));
    }

    /// A ListenBrainz submitter connected (a token in `listenbrainz.json`) at the stand-in, in use
    /// as the app's.
    fn use_listenbrainz(dir: &std::path::Path, m: &Mock) {
        listenbrainz::Account { server: String::new(), user: "alice".into(), token: LB_TOKEN.into() }.save(&dir.join("listenbrainz.json"), &listenbrainz::Sealer::PLAIN);
        let opts = listenbrainz::Options { root: Some(m.url.clone()), min_gap: Duration::from_millis(10), retry_min: Duration::from_millis(200), ..listenbrainz::Options::new(dir) };
        let rx = crate::connections::listens::start_for_test(opts);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let st = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("connected");
            if matches!(st.state, listenbrainz::State::Connected { .. }) {
                crate::connections::listens::use_status(Some(st));
                break;
            }
        }
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let t0 = Instant::now();
        while !f() {
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// "Scrobble server tracks from this app" off: a track played from the server sends Last.fm
    /// and ListenBrainz nothing (no now playing, no play), nor a file the server also has while
    /// its plays go to the server; a track the server does not have goes to both as ever. On:
    /// every track does. Then both at once with one of them down or hanging: the other gets its
    /// requests at once.
    #[test]
    fn both_services_hear_of_a_play_and_the_server_switch_covers_both() {
        let (url, log) = lastfm_mock();
        let dir = std::env::temp_dir().join(format!("mp-scrobble-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        use_lastfm(&dir, &url);
        let lb = Mock::start(LB_TOKEN, "alice");
        use_listenbrainz(&dir, &lb);
        let track = |title: &str, path: &str, remote: Option<&str>| Track {
            title: title.into(),
            artist: "Ann".into(),
            album: "Night".into(),
            album_artist: "Ann".into(),
            duration: 200.0,
            path: path.into(),
            remote_id: remote.map(str::to_owned),
            ..Track::default()
        };
        let server_only = track("From the server", &library::remote::song_path("s1"), Some("s1"));
        let matched = track("File the server has", "M:/a/02.flac", Some("s2"));
        let local = track("Only here", "M:/a/01.flac", None);
        let db = library::db::open_in_memory().unwrap();
        db.execute("INSERT INTO remote_server (id, identity, connected_at) VALUES (1, 'https://nas (ann)', 0)", []).unwrap();
        let play = |s: &crate::settings::Settings, t: &Track| {
            if let Some(p) = track_started(t, allowed(s, t, &db), None) {
                scrobble(p);
            }
        };
        // Sorted: the worker may send a queued play before the "now playing" waiting behind it
        // (a loaded machine).
        let sent = |title: &str| {
            let mut m = log.lock().unwrap().iter().filter(|f| f.values().any(|v| v == title)).map(|f| f["method"].clone()).collect::<Vec<_>>();
            m.sort();
            m
        };
        // ListenBrainz: (playing_now, stored) of a title.
        let heard = |title: &str| {
            lb.with(|s| {
                let n = |v: &Vec<serde_json::Value>| v.iter().filter(|l| l["track_metadata"]["track_name"] == title).count();
                (n(&s.playing_now), n(&s.listens))
            })
        };
        let off = crate::settings::Settings { server_lastfm: false, ..Default::default() };
        for t in [&server_only, &matched, &local] {
            play(&off, t);
        }
        wait_until("the local track on both", || sent("Only here").len() == 2 && heard("Only here") == (1, 1));
        // The last one sent is in: anything sent before it has been too.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(sent("Only here"), ["track.scrobble", "track.updateNowPlaying"], "a local track: now playing and its scrobble");
        assert!(sent("From the server").is_empty() && heard("From the server") == (0, 0), "a server track: nothing to either");
        assert!(sent("File the server has").is_empty() && heard("File the server has") == (0, 0), "a file whose play the server hears of: nothing");
        assert_eq!(lb.with(|s| s.requests.len()), 2, "ListenBrainz: one now playing and one listen");
        // Plays not reported to the server: the file the server has is the app's to scrobble.
        let not_reported = crate::settings::Settings { server_plays: false, ..off.clone() };
        assert!(!allowed(&not_reported, &server_only, &db) && allowed(&not_reported, &matched, &db) && allowed(&not_reported, &local, &db));
        // Plex never receives plays. A matched file plays locally and can still scrobble.
        db.execute("UPDATE remote_server SET identity = 'plex:abc'", []).unwrap();
        assert!(allowed(&off, &matched, &db));
        assert!(!allowed(&off, &server_only, &db));
        // On (the default): every track, as before.
        let on = crate::settings::Settings::default();
        play(&on, &server_only);
        wait_until("the server track on both", || sent("From the server").len() == 2 && heard("From the server") == (1, 1));
        assert_eq!(sent("From the server"), ["track.scrobble", "track.updateNowPlaying"]);

        // Last.fm down (nothing listens where it is): ListenBrainz gets its play at once, and
        // Last.fm keeps it for later.
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let gone_url = format!("http://127.0.0.1:{}/2.0/", gone.local_addr().unwrap().port());
        drop(gone);
        use_lastfm(&dir, &gone_url);
        let t0 = Instant::now();
        play(&on, &track("While Last.fm is down", "M:/a/03.flac", None));
        wait_until("ListenBrainz's", || heard("While Last.fm is down") == (1, 1));
        assert!(t0.elapsed() < Duration::from_secs(5), "not held up by Last.fm: {:?}", t0.elapsed());
        wait_until("Last.fm's queued", || SCROBBLER.with(|c| c.borrow().as_ref().is_some_and(|s| s.queued() == 1)));

        // ListenBrainz hanging (a server that takes the request and never answers): Last.fm gets
        // its now playing and scrobble at once all the same.
        use_lastfm(&dir, &url);
        lb.set_trouble(Trouble::Silent);
        let t0 = Instant::now();
        play(&on, &track("While ListenBrainz hangs", "M:/a/04.flac", None));
        wait_until("Last.fm's", || sent("While ListenBrainz hangs").len() == 2);
        assert!(t0.elapsed() < Duration::from_secs(5), "not held up by ListenBrainz: {:?}", t0.elapsed());
        assert_eq!(heard("While ListenBrainz hangs"), (0, 0));
        assert!(lb.with(|s| s.violations.is_empty()));
        SCROBBLER.with(|c| c.borrow_mut().take());
        crate::connections::listens::use_status(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Connected, then disconnected: the scrobbler's status keeps no user and takes no play, the
    /// fields Settings fills at start are empty, and nothing of the key, secret or session is
    /// left in the data folder. The same for a key and secret left by an older version.
    #[test]
    fn disconnect_leaves_nothing_for_settings_to_show() {
        let (url, _log) = lastfm_mock();
        let dir = std::env::temp_dir().join(format!("mp-scrobble-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        use_lastfm(&dir, &url);
        assert_eq!(saved_fields(&dir, &lastfm::Sealer::PLAIN), ("testkey0123456789".into(), "testsecret".into()));
        with(Scrobbler::disconnect);
        wait_until("forgotten", || !dir.join("lastfm.json").exists());
        assert_eq!(saved_fields(&dir, &lastfm::Sealer::PLAIN), (String::new(), String::new()), "a restart shows empty fields");
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).into_owned();
            assert!(!text.contains("testkey0123456789") && !text.contains("testsecret") && !text.contains("sk-1"), "{:?}", e.file_name());
        }
        // An older version's file: the key and secret, no session.
        std::fs::write(dir.join("lastfm.json"), r#"{"api_key":"oldkey","api_secret":"oldsecret","username":""}"#).unwrap();
        assert_eq!(saved_fields(&dir, &lastfm::Sealer::PLAIN), ("oldkey".into(), "oldsecret".into()));
        SCROBBLER.with(|c| c.borrow_mut().take());
        let opts = lastfm::Options { root: url.clone(), ..lastfm::Options::new(&dir) };
        SCROBBLER.with(|c| *c.borrow_mut() = Some(Scrobbler::start(opts, Box::new(|_| None), Box::new(|_| {}), Box::new(|_| {}))));
        with(Scrobbler::disconnect);
        wait_until("the older file forgotten", || !dir.join("lastfm.json").exists());
        assert_eq!(saved_fields(&dir, &lastfm::Sealer::PLAIN), (String::new(), String::new()));
        SCROBBLER.with(|c| c.borrow_mut().take());
        STATUS.with(|c| c.borrow_mut().take());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rule_is_half_or_four_minutes() {
        assert!(!counts(30.0, 30.0), "30 s or less never counts");
        assert!(!counts(100.0, 49.9));
        assert!(counts(100.0, 50.0), "half");
        assert!(counts(1200.0, 240.0), "four minutes, for a long track");
        assert!(!counts(1200.0, 239.0));
    }

    #[test]
    fn a_play_carries_the_track() {
        let t = Track { title: "Lantern".into(), artist: "A".into(), album: "N".into(), album_artist: "A".into(), track_no: Some(3), duration: 245.4, path: "M:/a.flac".into(), ..Track::default() };
        let s = scrobble_of(&t, 1_700_000_000);
        assert_eq!((s.duration, s.track_number, s.timestamp, s.path.as_deref()), (245, Some(3), 1_700_000_000, Some("M:/a.flac")));
        // The credit as tagged; several artists joined: the first.
        let t = Track { artist: "A feat. B".into(), album_artist: "A; C".into(), ..t };
        let s = scrobble_of(&t, 1);
        assert_eq!((s.artist.as_str(), s.album_artist.as_str()), ("A feat. B", "A"));
        // Only on the server: scrobbled the same, without a file to read.
        let t = Track { path: library::remote::song_path("s1"), ..t };
        let s = scrobble_of(&t, 1);
        assert_eq!((s.path, s.track.as_str(), s.duration), (None, "Lantern", 245));
    }
}
