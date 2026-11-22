//! A stand-in for a Navidrome server on 127.0.0.1, for the app's tests and its bench (feature
//! `mock`; never in a release build): no request made against it leaves the machine.
//!
//! It checks every request's authentication as Navidrome does (the user, and the token =
//! md5(password + salt); a plain password is refused) and answers the calls a library sync and
//! its covers make, shaped as Navidrome's JSON (the client's own models, serialized): `ping`,
//! `getScanStatus`, `getAlbumList2` (every `type` lists by name, paged by `size` and `offset`,
//! at most 500), `getAlbum`, `getCoverArt`, `stream` (a song's bytes, with ranges), and the
//! user's own data on songs as Navidrome keeps it: `getStarred2`, `star` / `unstar` (a song's
//! `starred` time) and `scrobble` (each kept in
//! [`Library::scrobbles`]; a play counts on the song's `playCount`). What it serves can be
//! changed while it runs ([`Mock::edit`]), and it can be made to misbehave ([`Trouble`]) or slow
//! ([`Mock::set_delay_ms`]). It keeps the endpoint and `id`s of every request
//! ([`Mock::requests`]: "star s1,s2"), never the secrets.
//!
//! The crate's own tests (`tests/mock.rs`) keep their fuller mock of every endpoint.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::models::{Album, Song};

/// What the mock serves.
#[derive(Clone, Debug, Default)]
pub struct Library {
    /// Albums with their songs (`song` filled); listed without them.
    pub albums: Vec<Album>,
    /// Cover bytes by cover art id; an id not here is a 404, as Navidrome answers.
    pub covers: HashMap<String, Vec<u8>>,
    /// Song bytes by song id, for `stream`.
    pub files: HashMap<String, Vec<u8>>,
    /// `getScanStatus`'s `lastScan`.
    pub last_scan: Option<String>,
    pub scanning: bool,
    /// Every `scrobble` taken, in order.
    pub scrobbles: Vec<Scrobbled>,
}

/// A `scrobble` the mock took: the song, its `time` (Unix ms) when sent, and whether it was a
/// play (`submission=true`) or "now playing".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scrobbled {
    pub id: String,
    pub time: Option<i64>,
    pub submission: bool,
}

impl Library {
    /// A song by id, to change what the server says of it (its star).
    pub fn song_mut(&mut self, id: &str) -> Option<&mut Song> {
        self.albums.iter_mut().flat_map(|a| a.song.iter_mut()).find(|s| s.id == id)
    }

    pub fn song(&self, id: &str) -> Option<&Song> {
        self.albums.iter().flat_map(|a| a.song.iter()).find(|s| s.id == id)
    }
}

/// How the mock misbehaves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Trouble {
    #[default]
    None,
    /// Every connection closed at once, unanswered: a server gone (or a proxy with nothing
    /// behind it).
    Down,
    /// HTTP 503 with a proxy's page.
    Unavailable,
    /// Code 40 whatever the token (the password was changed on the server).
    WrongPassword,
    /// `getAlbum` of this album fails with a server error (code 0).
    FailAlbum(String),
    /// The server fails to render one cover while the other images remain available.
    FailCover(String),
    /// `getAlbumList2` ignores `offset`: every page is the first.
    IgnoreOffset,
}

struct Shared {
    user: String,
    password: String,
    library: Mutex<Library>,
    trouble: Mutex<Trouble>,
    delay_ms: AtomicU64,
    requests: Mutex<Vec<String>>,
    stopped: AtomicBool,
}

/// A running mock. Dropping it stops answering.
pub struct Mock {
    /// The base URL to sign in with ("http://127.0.0.1:<port>").
    pub url: String,
    shared: Arc<Shared>,
}

impl Mock {
    /// Listen on a free port of 127.0.0.1, for `user` signed in with `password`.
    pub fn start(user: &str, password: &str, library: Library) -> std::io::Result<Mock> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let shared = Arc::new(Shared {
            user: user.to_owned(),
            password: password.to_owned(),
            library: Mutex::new(library),
            trouble: Mutex::new(Trouble::None),
            delay_ms: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
        });
        let s = shared.clone();
        std::thread::Builder::new().name("subsonic-mock".into()).spawn(move || {
            for conn in listener.incoming() {
                if s.stopped.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let s = s.clone();
                let _ = std::thread::Builder::new().name("subsonic-mock-conn".into()).spawn(move || answer(conn, &s));
            }
        })?;
        Ok(Mock { url, shared })
    }

    /// Change what is served (an album added, a scan finished...).
    pub fn edit(&self, f: impl FnOnce(&mut Library)) {
        f(&mut self.shared.library.lock().unwrap_or_else(|e| e.into_inner()));
    }

    pub fn set_trouble(&self, t: Trouble) {
        *self.shared.trouble.lock().unwrap_or_else(|e| e.into_inner()) = t;
    }

    /// Wait this long before each answer.
    pub fn set_delay_ms(&self, ms: u64) {
        self.shared.delay_ms.store(ms, Ordering::Relaxed);
    }

    /// Every request so far: "endpoint" or "endpoint id", in order.
    pub fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How many requests were made to `endpoint`.
    pub fn count(&self, endpoint: &str) -> usize {
        self.requests().iter().filter(|r| r.split(' ').next() == Some(endpoint)).count()
    }

    pub fn clear_requests(&self) {
        self.shared.requests.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

/// An album as a library holds it, with its songs, from a few facts (for tests and the bench):
/// `songs` are (title, track, seconds, size, path). Ids are `<id>-<n>`; the album's cover art id
/// is `al-<id>`.
pub fn album(id: &str, artist: &str, title: &str, year: i32, created: &str, songs: &[(&str, u32, u32, u64, &str)]) -> Album {
    let song = |(i, (t, n, secs, size, path)): (usize, &(&str, u32, u32, u64, &str))| Song {
        id: format!("{id}-{}", i + 1),
        parent: Some(id.to_owned()),
        title: (*t).to_owned(),
        album: title.to_owned(),
        artist: artist.to_owned(),
        album_id: Some(id.to_owned()),
        track: Some(*n),
        disc_number: Some(1),
        year: Some(year),
        genre: "Rock".into(),
        cover_art: Some(format!("al-{id}")),
        size: Some(*size),
        content_type: "audio/flac".into(),
        suffix: "flac".into(),
        duration: Some(*secs),
        bit_rate: Some(900),
        bit_depth: Some(16),
        sampling_rate: Some(44_100),
        channel_count: Some(2),
        path: (*path).to_owned(),
        created: Some(created.to_owned()),
        kind: "music".into(),
        media_type: "song".into(),
        genres: vec![crate::models::Named { name: "Rock".into() }],
        artists: vec![crate::models::ArtistRef { id: format!("ar-{artist}"), name: artist.to_owned() }],
        display_artist: artist.to_owned(),
        album_artists: vec![crate::models::ArtistRef { id: format!("ar-{artist}"), name: artist.to_owned() }],
        display_album_artist: artist.to_owned(),
        ..Song::default()
    };
    Album {
        id: id.to_owned(),
        name: title.to_owned(),
        artist: artist.to_owned(),
        artist_id: Some(format!("ar-{artist}")),
        cover_art: Some(format!("al-{id}")),
        song_count: songs.len() as u32,
        duration: songs.iter().map(|s| s.2).sum(),
        created: Some(created.to_owned()),
        year: Some(year),
        genre: "Rock".into(),
        genres: vec![crate::models::Named { name: "Rock".into() }],
        artists: vec![crate::models::ArtistRef { id: format!("ar-{artist}"), name: artist.to_owned() }],
        display_artist: artist.to_owned(),
        release_types: vec!["album".into()],
        song: songs.iter().enumerate().map(song).collect(),
        ..Album::default()
    }
}

// ---- answering ------------------------------------------------------------------------

struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<String>,
    body: Vec<u8>,
}

const ENVELOPE: &str = r#""status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.53.3 (mock)","openSubsonic":true"#;

fn ok(inner: &str) -> Reply {
    let sep = if inner.is_empty() { "" } else { "," };
    Reply { status: 200, content_type: "application/json", headers: Vec::new(), body: format!(r#"{{"subsonic-response":{{{ENVELOPE}{sep}{inner}}}}}"#).into_bytes() }
}

fn failed(code: i64, message: &str) -> Reply {
    let body = serde_json::json!({"subsonic-response": {"status": "failed", "version": "1.16.1", "type": "navidrome", "serverVersion": "0.53.3 (mock)", "openSubsonic": true, "error": {"code": code, "message": message}}});
    Reply { status: 200, content_type: "application/json", headers: Vec::new(), body: body.to_string().into_bytes() }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn answer(mut conn: TcpStream, s: &Shared) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(clone) = conn.try_clone() else { return };
    let mut r = BufReader::new(clone);
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut range: Option<u64> = None;
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "range" {
                range = v.trim().strip_prefix("bytes=").and_then(|v| v.trim_end_matches('-').parse().ok());
            } else if k == "content-length" {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length];
    let _ = r.read_exact(&mut body);
    let target = line.split(' ').nth(1).unwrap_or("").to_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut params: Vec<(String, String)> = Vec::new();
    for q in [query, std::str::from_utf8(&body).unwrap_or("")] {
        params.extend(q.split('&').filter(|kv| !kv.is_empty()).map(|kv| kv.split_once('=').unwrap_or((kv, ""))).map(|(k, v)| (decode(k), decode(v))));
    }
    let param = |k: &str| get(&params, k);
    let endpoint = path.rsplit('/').next().unwrap_or("").trim_end_matches(".view").to_owned();
    let ids: Vec<&str> = params.iter().filter(|(k, _)| k == "id").map(|(_, v)| v.as_str()).collect();
    s.requests.lock().unwrap_or_else(|e| e.into_inner()).push(if ids.is_empty() { endpoint.clone() } else { format!("{endpoint} {}", ids.join(",")) });
    let delay = s.delay_ms.load(Ordering::Relaxed);
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }
    let trouble = s.trouble.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (salt, token) = (param("s").unwrap_or(""), param("t").unwrap_or(""));
    let auth_ok = param("u") == Some(s.user.as_str()) && !salt.is_empty() && token == crate::token(&s.password, salt) && param("p").is_none();
    let reply = match trouble {
        Trouble::Down => return,
        Trouble::Unavailable => Reply { status: 503, content_type: "text/html", headers: Vec::new(), body: b"<html>503 Service Unavailable</html>".to_vec() },
        Trouble::WrongPassword => failed(40, "Wrong username or password"),
        _ if !auth_ok => failed(40, "Wrong username or password"),
        Trouble::FailAlbum(id) if endpoint == "getAlbum" && param("id") == Some(id.as_str()) => failed(0, "database is locked"),
        Trouble::FailCover(id) if endpoint == "getCoverArt" && param("id") == Some(id.as_str()) => Reply {
            status: 500, content_type: "text/plain", headers: Vec::new(), body: b"thumbnail unavailable".to_vec(),
        },
        Trouble::IgnoreOffset if endpoint == "getAlbumList2" => {
            let first: Vec<(String, String)> = params.iter().filter(|(k, _)| k != "offset").cloned().collect();
            serve(&endpoint, &first, range, &mut s.library.lock().unwrap_or_else(|e| e.into_inner()))
        }
        _ => serve(&endpoint, &params, range, &mut s.library.lock().unwrap_or_else(|e| e.into_inner())),
    };
    let mut head = format!("HTTP/1.1 {} X\r\nContent-Type: {}\r\nConnection: close\r\nContent-Length: {}\r\n", reply.status, reply.content_type, reply.body.len());
    for h in &reply.headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(&reply.body);
    let _ = conn.flush();
}

fn listed(a: &Album) -> Album {
    Album { song: Vec::new(), ..a.clone() }
}

fn get<'a>(params: &'a [(String, String)], k: &str) -> Option<&'a str> {
    params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// An ISO 8601 UTC time as Navidrome writes one, from Unix milliseconds.
pub fn iso(ms: i64) -> String {
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s, frac) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:03}Z")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn serve(endpoint: &str, params: &[(String, String)], range: Option<u64>, lib: &mut Library) -> Reply {
    let param = |k: &str| get(params, k);
    let all = |k: &str| -> Vec<String> { params.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect() };
    let json = |key: &str, v: serde_json::Value| ok(&format!("\"{key}\":{v}"));
    match endpoint {
        // Songs only (the app stars and rates songs); an id the server does not have is a
        // 70, as Navidrome answers.
        "star" | "unstar" => {
            let ids = all("id");
            if ids.iter().any(|id| lib.song(id).is_none()) {
                return failed(70, "Song not found");
            }
            let at = (endpoint == "star").then(|| iso(now_ms()));
            for id in ids {
                if let Some(s) = lib.song_mut(&id) {
                    s.starred = at.clone();
                }
            }
            ok("")
        }
        "scrobble" => {
            let ids = all("id");
            let times: Vec<i64> = all("time").iter().filter_map(|t| t.parse().ok()).collect();
            let submission = param("submission") != Some("false");
            if ids.is_empty() || ids.iter().any(|id| lib.song(id).is_none()) {
                return failed(70, "Song not found");
            }
            for (i, id) in ids.into_iter().enumerate() {
                if submission && let Some(s) = lib.song_mut(&id) {
                    s.play_count = Some(s.play_count.unwrap_or(0) + 1);
                }
                lib.scrobbles.push(Scrobbled { id, time: times.get(i).copied(), submission });
            }
            ok("")
        }
        "getStarred2" => {
            let songs: Vec<&Song> = lib.albums.iter().flat_map(|a| a.song.iter()).filter(|s| s.starred.is_some()).collect();
            json("starred2", serde_json::json!({ "song": songs }))
        }
        "ping" => ok(""),
        "getOpenSubsonicExtensions" => ok(r#""openSubsonicExtensions":[{"name":"songLyrics","versions":[1]}]"#),
        "getScanStatus" => json(
            "scanStatus",
            serde_json::json!({"scanning": lib.scanning, "count": lib.albums.iter().map(|a| a.song.len()).sum::<usize>(), "folderCount": lib.albums.len(), "lastScan": lib.last_scan}),
        ),
        "getAlbumList2" => {
            let size = param("size").and_then(|v| v.parse::<usize>().ok()).unwrap_or(10).min(500);
            let offset = param("offset").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let mut all: Vec<&Album> = lib.albums.iter().collect();
            all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
            let page: Vec<Album> = all.into_iter().skip(offset).take(size).map(listed).collect();
            json("albumList2", serde_json::json!({ "album": page }))
        }
        "getAlbum" => match lib.albums.iter().find(|a| Some(a.id.as_str()) == param("id")) {
            Some(a) => json("album", serde_json::to_value(a).unwrap_or_default()),
            None => failed(70, "Album not found"),
        },
        "getCoverArt" => match param("id").and_then(|id| lib.covers.get(id)) {
            Some(bytes) => Reply { status: 200, content_type: "image/png", headers: Vec::new(), body: bytes.clone() },
            None => Reply { status: 404, content_type: "text/plain", headers: Vec::new(), body: b"404 page not found".to_vec() },
        },
        "stream" => match param("id").and_then(|id| lib.files.get(id)) {
            Some(bytes) => {
                let from = range.unwrap_or(0).min(bytes.len() as u64) as usize;
                let total = bytes.len();
                if range.is_some() && from > 0 {
                    Reply {
                        status: 206,
                        content_type: "audio/flac",
                        headers: vec![format!("Content-Range: bytes {from}-{}/{total}", total.saturating_sub(1)), "Accept-Ranges: bytes".into()],
                        body: bytes[from..].to_vec(),
                    }
                } else {
                    Reply { status: 200, content_type: "audio/flac", headers: vec!["Accept-Ranges: bytes".into()], body: bytes.clone() }
                }
            }
            None => failed(70, "Song not found"),
        },
        _ => failed(0, "not implemented by the mock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AlbumList, Client, Credentials, Error};

    #[test]
    fn the_mock_answers_a_client() {
        let mut lib = Library { last_scan: Some("2026-11-01T00:00:00Z".into()), ..Default::default() };
        lib.albums = (0..7).map(|i| album(&format!("a{i}"), "Ann", &format!("Album {i}"), 2000 + i, "2026-01-01T00:00:00Z", &[("One", 1, 100, 1000, "Ann/x/01 One.flac")])).collect();
        lib.covers.insert("al-a1".into(), vec![1, 2, 3]);
        let m = Mock::start("ann", "pw", lib).unwrap();
        let (creds, info) = crate::sign_in(&m.url, "ann", "pw").unwrap();
        assert_eq!(info.kind, "navidrome");
        let c = Client::new(creds.clone(), true).unwrap();
        assert_eq!(c.scan_status().unwrap().last_scan.as_deref(), Some("2026-11-01T00:00:00Z"));
        let p1 = c.album_list2(&AlbumList::AlphabeticalByName, 5, 0, None).unwrap();
        let p2 = c.album_list2(&AlbumList::AlphabeticalByName, 5, 5, None).unwrap();
        assert_eq!((p1.len(), p2.len()), (5, 2));
        assert!(p1[0].song.is_empty());
        let a = c.album("a3").unwrap();
        assert_eq!((a.song.len(), a.song[0].path.as_str(), a.song[0].size), (1, "Ann/x/01 One.flac", Some(1000)));
        assert_eq!(c.cover_art("al-a1", None).unwrap().bytes, vec![1, 2, 3]);
        assert!(matches!(c.cover_art("al-a2", None), Err(Error::NotFound(_))));
        assert!(matches!(c.album("nope"), Err(Error::NotFound(_))));
        assert!(matches!(crate::sign_in(&m.url, "ann", "wrong"), Err(Error::WrongCredentials { .. })));
        m.set_trouble(Trouble::FailAlbum("a2".into()));
        assert!(matches!(c.album("a2"), Err(Error::Server { .. })));
        m.set_trouble(Trouble::Down);
        assert!(matches!(c.ping(), Err(Error::Network(_))));
        m.set_trouble(Trouble::None);
        assert!(m.count("getAlbum") >= 3);
        assert!(m.requests().iter().all(|r| !r.contains(&creds.token)));
        let bad = Client::new(Credentials { token: "x".into(), ..creds }, true).unwrap();
        assert!(matches!(bad.ping(), Err(Error::WrongCredentials { .. })));
    }

    #[test]
    fn times_are_written_as_navidrome_writes_them() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_714_557_600_123), "2024-05-01T10:00:00.123Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn the_mock_keeps_stars_and_plays() {
        let lib = Library { albums: vec![album("a", "Ann", "A", 2000, "2026-01-01T00:00:00Z", &[("One", 1, 100, 1000, ""), ("Two", 2, 100, 1000, "")])], ..Default::default() };
        let m = Mock::start("ann", "pw", lib).unwrap();
        let (creds, _) = crate::sign_in(&m.url, "ann", "pw").unwrap();
        let c = Client::new(creds, false).unwrap();
        c.star(&["a-1", "a-2"], &[], &[]).unwrap();
        c.unstar(&["a-2"], &[], &[]).unwrap();
        c.scrobble("a-1", Some(1_000), true).unwrap();
        c.scrobble("a-2", None, false).unwrap();
        assert!(matches!(c.star(&["nope"], &[], &[]), Err(Error::NotFound(_))));
        let s = c.album("a").unwrap().song;
        assert!(s[0].starred.is_some() && s[1].starred.is_none());
        assert_eq!((s[0].play_count, s[1].play_count), (Some(1), None));
        assert_eq!(c.starred2(None).unwrap().song.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a-1"]);
        m.edit(|l| assert_eq!(l.scrobbles, [Scrobbled { id: "a-1".into(), time: Some(1_000), submission: true }, Scrobbled { id: "a-2".into(), time: None, submission: false }]));
        assert_eq!(m.requests()[1], "star a-1,a-2");
    }
}
