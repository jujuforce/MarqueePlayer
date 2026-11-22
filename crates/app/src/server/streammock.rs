//! A local stand-in for a Navidrome server, on 127.0.0.1 only, for the streaming tests and
//! `MP_BENCH=streaming` (bench/streaming.rs): `stream` of the original file with ranges as Navidrome
//! serves `format=raw` (206 with `Content-Range`, 416 past the end; a `Last-Modified` on each
//! answer and an `If-Range` honoured, as Go's `http.ServeContent` does there), `ping`, and
//! `getLyricsBySongId`; and on demand a throttled link, a stall at an offset, a connection
//! dropped mid-body, 503s, ranges ignored, and songs that are gone (404). It checks each
//! request's token as the server does (md5(password + salt)), and keeps what it was asked and
//! how many connections are open. Fixtures are made by ffmpeg (`fixtures`). Nothing here
//! reaches another machine.

// Each user (the tests, the bench) uses its own part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const USER: &str = "bench";
pub const PASSWORD: &str = "not a real password";

/// How the next requests are answered.
#[derive(Clone, Debug, Default)]
pub struct Behaviour {
    /// Bytes a second, each connection; 0: as fast as it goes.
    pub rate: u64,
    /// Once: at this offset of the file, send nothing for this long (then go on), or until the
    /// client gives up on the connection (then nothing more is sent on it).
    pub stall_at: Option<(u64, Duration)>,
    /// Once: close the connection once the body reaches this offset of the file.
    pub drop_at: Option<u64>,
    /// Once: when a body reaches this offset of the file, the song is replaced by these bytes
    /// (as `Mock::replace`), before any more is sent. The answer under way goes on with the old
    /// file; the requests after it get the new one.
    pub replace_at: Option<(u64, Vec<u8>)>,
    /// The next this many stream requests answer 503.
    pub fail_next: usize,
    /// Answer every range with the whole file (200), as some servers do.
    pub ignore_ranges: bool,
}

/// One request as the mock read it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub endpoint: String,
    pub id: String,
    /// The range asked (0 without one).
    pub from: u64,
    /// The `If-Range` sent, if one was.
    pub if_range: Option<String>,
    pub at: Instant,
    pub auth_ok: bool,
}

#[derive(Clone)]
struct Song {
    bytes: Arc<Vec<u8>>,
    content_type: String,
    /// Its `Last-Modified`: another for each version of the file (`replace`).
    modified: String,
}

/// The `Last-Modified` of a song's `n`th version (HTTP dates, a day apart).
fn modified(n: usize) -> String {
    format!("Wed, {:02} Oct 2026 07:28:00 GMT", 1 + n % 28)
}

#[derive(Default)]
struct Shared {
    songs: Mutex<HashMap<String, Song>>,
    mp3s: Mutex<HashMap<(String, u32), Song>>,
    lyrics: Mutex<HashMap<String, String>>,
    behaviour: Mutex<Behaviour>,
    seen: Mutex<Vec<Seen>>,
    open: AtomicUsize,
    /// What each open connection is doing (for a report when one lingers).
    doing: Mutex<HashMap<usize, String>>,
    next_conn: AtomicUsize,
    /// Body bytes written, all connections.
    sent: AtomicUsize,
    quit: AtomicBool,
}

pub struct Mock {
    pub url: String,
    shared: Arc<Shared>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

impl Mock {
    pub fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let shared: Arc<Shared> = Arc::default();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("mock-navidrome".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if s.quit.load(Ordering::Acquire) {
                        return;
                    }
                    let Ok(conn) = conn else { continue };
                    let s = s.clone();
                    s.open.fetch_add(1, Ordering::SeqCst);
                    let n = s.next_conn.fetch_add(1, Ordering::SeqCst);
                    s.doing.lock().unwrap().insert(n, "reading the request".into());
                    std::thread::spawn(move || {
                        answer(conn, &s, n);
                        s.doing.lock().unwrap().remove(&n);
                        s.open.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            })
            .expect("the mock's thread");
        Mock { url, shared }
    }

    pub fn add(&self, id: &str, bytes: Vec<u8>, content_type: &str) {
        self.shared.songs.lock().unwrap().insert(id.to_owned(), Song { bytes: Arc::new(bytes), content_type: content_type.to_owned(), modified: modified(0) });
    }

    #[cfg(test)]
    pub fn add_mp3(&self, id: &str, bitrate: u32, bytes: Vec<u8>) {
        self.shared.mp3s.lock().unwrap().insert((id.to_owned(), bitrate), Song { bytes: Arc::new(bytes), content_type: "audio/mpeg".to_owned(), modified: String::new() });
    }

    /// The song's file replaced on the server by `bytes`, under the same id: another
    /// `Last-Modified` from here on.
    pub fn replace(&self, id: &str, bytes: Vec<u8>) {
        replace(&self.shared, id, bytes);
    }

    pub fn remove(&self, id: &str) {
        self.shared.songs.lock().unwrap().remove(id);
    }

    /// The server's lyrics for a song: the `structuredLyrics` array, as JSON.
    pub fn set_lyrics(&self, id: &str, structured: &str) {
        self.shared.lyrics.lock().unwrap().insert(id.to_owned(), structured.to_owned());
    }

    pub fn set(&self, f: impl FnOnce(&mut Behaviour)) {
        f(&mut self.shared.behaviour.lock().unwrap());
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// The stream requests, in order.
    pub fn streams(&self) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| s.endpoint == "stream").collect()
    }

    pub fn streams_of(&self, id: &str) -> Vec<Seen> {
        self.streams().into_iter().filter(|s| s.id == id).collect()
    }

    /// Connections open now.
    pub fn open(&self) -> usize {
        self.shared.open.load(Ordering::SeqCst)
    }

    /// What the open connections are doing.
    pub fn doing(&self) -> Vec<String> {
        self.shared.doing.lock().unwrap().values().cloned().collect()
    }

    pub fn sent(&self) -> usize {
        self.shared.sent.load(Ordering::SeqCst)
    }

    pub fn credentials(&self) -> subsonic::Credentials {
        subsonic::Credentials::from_password(&self.url, USER, PASSWORD).expect("credentials")
    }

    /// A read-only client for it, as the app makes one.
    pub fn client(&self) -> super::provider::Client {
        super::provider::Client::Subsonic(subsonic::Client::new(self.credentials(), true).expect("client"))
    }
}

fn replace(s: &Shared, id: &str, bytes: Vec<u8>) {
    let mut songs = s.songs.lock().unwrap();
    let song = songs.get_mut(id).expect("a song added before");
    let next = (1..).map(modified).find(|m| *m != song.modified).unwrap_or_default();
    *song = Song { bytes: Arc::new(bytes), content_type: song.content_type.clone(), modified: next };
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
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

const ENVELOPE: &str = r#""status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.53.3 (mock)","openSubsonic":true"#;

fn write_head(conn: &mut TcpStream, status: u16, content_type: &str, headers: &[String], length: usize) -> bool {
    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\nContent-Length: {length}\r\n");
    for h in headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    conn.write_all(head.as_bytes()).is_ok()
}

fn reply(conn: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    if write_head(conn, status, content_type, &[], body.len()) {
        let _ = conn.write_all(body);
    }
}

fn answer(mut conn: TcpStream, s: &Shared, n: usize) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    // As a real server sends: each paced piece at once, not held back for the last one's
    // acknowledgement (which macOS delays, slowing the rate the tests set).
    let _ = conn.set_nodelay(true);
    let mut r = BufReader::new(match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let target = line.split(' ').nth(1).unwrap_or("").to_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let params: Vec<(String, String)> = query.split('&').filter(|kv| !kv.is_empty()).map(|kv| kv.split_once('=').unwrap_or((kv, ""))).map(|(k, v)| (decode(k), decode(v))).collect();
    let param = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let endpoint = path.strip_prefix("/rest/").unwrap_or("").trim_end_matches(".view").to_owned();
    let (salt, token) = (param("s").unwrap_or_default(), param("t").unwrap_or_default());
    let auth_ok = param("u").as_deref() == Some(USER) && !salt.is_empty() && token == subsonic::token(PASSWORD, &salt) && param("p").is_none();
    let id = param("id").unwrap_or_default();
    let from = headers.get("range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split('-').next()).and_then(|n| n.trim().parse::<u64>().ok()).unwrap_or(0);
    let if_range = headers.get("if-range").cloned();
    s.seen.lock().unwrap().push(Seen { endpoint: endpoint.clone(), id: id.clone(), from, if_range: if_range.clone(), at: Instant::now(), auth_ok });
    if !auth_ok {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome","serverVersion":"0.53.3","openSubsonic":true,"error":{"code":40,"message":"Wrong username or password"}}}"#;
        return reply(&mut conn, 200, "application/json", body.as_bytes());
    }
    match endpoint.as_str() {
        "ping" => reply(&mut conn, 200, "application/json", format!(r#"{{"subsonic-response":{{{ENVELOPE}}}}}"#).as_bytes()),
        "getLyricsBySongId" => {
            let l = s.lyrics.lock().unwrap().get(&id).cloned().unwrap_or_else(|| "[]".into());
            reply(&mut conn, 200, "application/json", format!(r#"{{"subsonic-response":{{{ENVELOPE},"lyricsList":{{"structuredLyrics":{l}}}}}}}"#).as_bytes())
        }
        "stream" => {
            let bitrate = match param("format").as_deref() {
                Some("raw") => None,
                Some("mp3") => Some(param("maxBitRate").unwrap().parse().unwrap()),
                _ => return reply(&mut conn, 400, "text/plain", b"unsupported format"),
            };
            stream(&mut conn, s, &id, from, if_range.as_deref(), bitrate, n)
        }
        _ => reply(&mut conn, 200, "application/json", br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"not in the mock"}}}"#),
    }
}

/// The client closed the connection (gave up on it): what it would read next is its end.
fn client_left(conn: &TcpStream) -> bool {
    if conn.set_nonblocking(true).is_err() {
        return false;
    }
    let mut b = [0u8; 1];
    let left = match conn.peek(&mut b) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
    };
    let _ = conn.set_nonblocking(false);
    left
}

fn stream(conn: &mut TcpStream, s: &Shared, id: &str, from: u64, if_range: Option<&str>, bitrate: Option<u32>, n: usize) {
    let song = match bitrate {
        Some(kbps) => s.mp3s.lock().unwrap().get(&(id.to_owned(), kbps)).cloned(),
        None => s.songs.lock().unwrap().get(id).cloned(),
    };
    let Some(song) = song else {
        return reply(conn, 404, "text/plain", b"404 page not found");
    };
    let b = {
        let mut b = s.behaviour.lock().unwrap();
        if b.fail_next > 0 {
            b.fail_next -= 1;
            drop(b);
            return reply(conn, 503, "text/plain", b"Service Unavailable");
        }
        b.clone()
    };
    let len = song.bytes.len() as u64;
    // A range of another version of the file than the one asked about: the whole file instead.
    let changed = if_range.is_some_and(|v| v != song.modified);
    let start = if bitrate.is_some() || b.ignore_ranges || changed { 0 } else { from };
    let lm = format!("Last-Modified: {}", song.modified);
    if start >= len && len > 0 {
        let _ = write_head(conn, 416, "text/plain", &[format!("Content-Range: bytes */{len}")], 0);
        return;
    }
    let ok = if bitrate.is_some() {
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").is_ok()
    } else if start > 0 {
        write_head(conn, 206, &song.content_type, &[format!("Content-Range: bytes {start}-{}/{len}", len - 1), "Accept-Ranges: bytes".into(), lm], (len - start) as usize)
    } else {
        write_head(conn, 200, &song.content_type, &["Accept-Ranges: bytes".into(), lm], len as usize)
    };
    if !ok {
        return;
    }
    let mut at = start;
    // When the next piece is due. Each piece is paced against this, not by a sleep of its own
    // length: a sleep that overruns (a loaded machine, macOS' coalesced timers) is made up by the
    // next ones, so the rate holds over the file.
    let mut due = Instant::now();
    while at < len {
        if s.quit.load(Ordering::Acquire) {
            return;
        }
        // Pieces of a twentieth of a second at the rate (16 KB at most), each paced on its own:
        // a rate changed meanwhile applies from the next piece, and a client that left is seen
        // within one.
        let rate = s.behaviour.lock().unwrap().rate;
        let piece = if rate > 0 { (rate / 20).clamp(512, 16 * 1024) } else { 16 * 1024 };
        let mut end = (at + piece).min(len);
        // A stall or a drop lands exactly at its offset: a piece ends there, the next starts
        // with it.
        let mut stall = None;
        {
            let mut cur = s.behaviour.lock().unwrap();
            match cur.stall_at {
                Some((o, d)) if o == at => {
                    stall = Some(d);
                    cur.stall_at = None;
                }
                Some((o, _)) if at < o && o < end => end = o,
                _ => {}
            }
            match &cur.replace_at {
                Some((o, _)) if *o == at => {
                    let (_, bytes) = cur.replace_at.take().unwrap();
                    replace(s, id, bytes);
                }
                Some((o, _)) if at < *o && *o < end => end = *o,
                _ => {}
            }
            match cur.drop_at {
                Some(o) if o == at => {
                    cur.drop_at = None;
                    let _ = conn.flush();
                    let _ = conn.shutdown(std::net::Shutdown::Both);
                    return;
                }
                Some(o) if at < o && o < end => end = o,
                _ => {}
            }
        }
        if let Some(d) = stall {
            let until = Instant::now() + d;
            while Instant::now() < until && !s.quit.load(Ordering::Acquire) {
                if client_left(conn) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            due = Instant::now();
        }
        s.doing.lock().unwrap().insert(n, format!("stream {id} from {from}: writing at {at}"));
        if bitrate.is_some() && write!(conn, "{:x}\r\n", end - at).is_err() {
            return;
        }
        if conn.write_all(&song.bytes[at as usize..end as usize]).is_err() || (bitrate.is_some() && conn.write_all(b"\r\n").is_err()) {
            return;
        }
        s.sent.fetch_add((end - at) as usize, Ordering::SeqCst);
        if rate > 0 {
            // At most a tenth of a second is made up: after a client that stopped reading for a
            // while, the rate is the rate again, not a burst.
            let now = Instant::now();
            due = (due + Duration::from_secs_f64((end - at) as f64 / rate as f64)).max(now.checked_sub(Duration::from_millis(100)).unwrap_or(now));
            if let Some(wait) = due.checked_duration_since(now) {
                std::thread::sleep(wait);
            }
        } else {
            due = Instant::now();
        }
        at = end;
    }
    if bitrate.is_some() {
        let _ = conn.write_all(b"0\r\n\r\n");
    }
    let _ = conn.flush();
}

// ---- fixtures ----------------------------------------------------------------------

pub fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// The formats the streaming tests play, by file name and ffmpeg's encoder arguments: FLAC and
/// MP3 open from their start, Opus needs its last page and an MP4 with `moov` after its audio
/// (ffmpeg's default without `+faststart`) needs its end before anything plays.
pub const FORMATS: [(&str, &[&str], &str); 4] = [
    ("song.flac", &["-c:a", "flac"], "audio/flac"),
    ("song.mp3", &["-c:a", "libmp3lame", "-b:a", "320k"], "audio/mpeg"),
    ("song.opus", &["-c:a", "libopus", "-b:a", "160k"], "audio/ogg"),
    ("song-moov-end.m4a", &["-c:a", "aac", "-b:a", "256k"], "audio/mp4"),
];

/// Make (once) the fixtures in `dir`: each of `FORMATS`, `seconds` long, and two FLAC halves of
/// one signal cut at a frame that is no codec's boundary (`a.flac`, `b.flac`, for gapless).
/// Returns the paths in `FORMATS`' order, then the halves.
pub fn fixtures(dir: &Path, seconds: u32) -> Vec<PathBuf> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::create_dir_all(dir);
    // A signal with a spectrum (tones, a little noise), so every encoder has work to do.
    let expr = "0.3*sin(2*PI*440*t)+0.1*sin(2*PI*3000*t)+0.02*(random(0)-0.5)|0.3*sin(2*PI*660*t)+0.05*sin(2*PI*5000*t)+0.02*(random(1)-0.5)";
    let src = format!("aevalsrc={expr}:s=44100:d={seconds}");
    let ffmpeg = |args: &[&str], out: &Path| {
        if out.exists() {
            return;
        }
        let tmp = out.with_extension(format!("tmp.{}", out.extension().and_then(|e| e.to_str()).unwrap_or("x")));
        let mut v: Vec<&str> = vec!["-hide_banner", "-loglevel", "error", "-y"];
        v.extend(args);
        let tmp_s = tmp.to_string_lossy().into_owned();
        v.push(&tmp_s);
        let ok = std::process::Command::new("ffmpeg").args(&v).status().is_ok_and(|s| s.success());
        assert!(ok, "ffmpeg failed making {}", out.display());
        std::fs::rename(&tmp, out).expect("fixture in place");
    };
    let mut out = Vec::new();
    for (name, args, _) in FORMATS {
        let p = dir.join(format!("{seconds}s-{name}"));
        let mut a = vec!["-f", "lavfi", "-i", src.as_str()];
        a.extend(args.iter().copied());
        ffmpeg(&a, &p);
        out.push(p);
    }
    let cut = 44_100 * seconds as u64 / 2 + 1_111;
    for (name, filter) in [("a.flac", format!("atrim=end_sample={cut}")), ("b.flac", format!("atrim=start_sample={cut},asetpts=PTS-STARTPTS"))] {
        let p = dir.join(format!("{seconds}s-{name}"));
        ffmpeg(&["-f", "lavfi", "-i", &src, "-af", &filter, "-c:a", "flac"], &p);
        out.push(p);
    }
    out
}
