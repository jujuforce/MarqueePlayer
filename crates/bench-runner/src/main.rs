//! Every bench mode (`MP_BENCH`) in sequence, each on a fresh copy of the fixtures it needs
//! and a fresh data folder, against local mocks of LRCLIB and Last.fm (nothing leaves this
//! machine), then one pass/fail table. The release's end-to-end evidence.
//!
//!     cargo build --release -p marquee-player --features bench
//!     cargo build --release -p library --examples
//!     cargo run --release -p bench-runner -- [--only a,b] [--skip a,b] [--keep]
//!
//! (`cargo xtask bench` does all three, and passes its arguments after `--` on.) Options:
//!
//! - `--exe <path>`: the app built with the bench feature (default: `MarqueePlayer[.exe]` beside
//!   this runner, in the same target folder);
//! - `--fixtures <dir>`: made by `bench_fixtures` if missing (default `<temp>/mp-bench-fixtures`);
//! - `--out <dir>`: one folder per mode with its data, music copy, reports, snapshots and log
//!   (default `<temp>/mp-bench-runs`; emptied first, and must be in the temporary folder);
//! - `--only`, `--skip`: modes by name; `--quick` skips the long ones (`LONG`); `--list` prints
//!   them; `--keep` keeps each mode's music copy.
//!
//! A mode passes when the app exits 0 (every check passed: `bench::exit_code`), fails on
//! exit 3 (failed checks; the report has `[FAIL]` lines), and is a crash on any other code
//! (101 is a panic) or a hang when it outlives its time limit (it is then killed: only that
//! child process, by its handle). The app runs with `MP_VOLUME=0`, `MP_WINDOW=secondary` and
//! every network address pointing at the mocks; the bench harness itself opens no native
//! dialog and writes nothing to the clipboard.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A bench mode and what it needs.
struct Mode {
    name: &'static str,
    /// `MP_BENCH`'s value (several entries can run one mode with different settings).
    bench: &'static str,
    /// Fixture sets copied into the music folder (`<music>/<set>`), in this order.
    sets: &'static [&'static str],
    /// Seconds before it counts as a hang.
    limit: u64,
    env: &'static [(&'static str, &'static str)],
    /// Runs off Windows too, on Linux and macOS (the rest need Windows: GDI counts, the tray's
    /// Win32 icon, WASAPI's device list, a resize without activation).
    linux: bool,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    /// `zoom` (the interface size, left at 150%), then `zoom-check`: the next start keeps it.
    Zoom,
    /// `session-save`, a folder moved, then `session-check` with a second folder to remove.
    Session,
    /// A first run scans, then nineteen in twenty files go: the guard must hold the tracks back.
    Guard,
    /// The first scan of a large library (the `big` set, scanned where it is, never copied): a
    /// run quit mid-scan, then one that must finish it.
    BigScan,
    /// The first scan of the messy library on a slow disk (simulated, `MP_SLOW_DISK_MS`): a run quit
    /// 40% through, then one that must pick it up and finish it.
    SlowScan,
    /// The app's own files misbehaving (`scenario`): launched as `idle-stopped`, checked here.
    Scenario(Scenario),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scenario {
    /// settings.json with values it cannot read, lastfm.json and scrobbles.json damaged,
    /// theme.toml broken, a junk record in the tag editor's `inflight`.
    DataFiles,
    /// library.db is not a database: refused with a message; then "start afresh" (answered yes).
    DbGarbage,
    /// library.db from a newer build: refused with a message, left untouched.
    DbNewer,
    /// library.db with bytes flipped inside (the header intact): what the app makes of it.
    DbFlipped,
    /// The data folder cannot be written to.
    DataReadOnly,
    /// Two launches at once on one data folder: one runs, the other hands over and leaves.
    TwoInstances,
    /// A deliberate panic on the UI thread, then on another: told, logged with a backtrace, the
    /// log kept as the previous one after the next start.
    Crash,
}

/// The modes that are long by design (minutes of playing, or scanning a large library): left out
/// by `--quick`, run by the full pass before a release.
const LONG: &[&str] = &["soak", "bigscan", "slowscan"];

const MODES: &[Mode] = &[
    Mode { name: "tour", bench: "tour", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "grid", bench: "grid", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "sidebar", bench: "sidebar", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "covers", bench: "covers", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "playlist", bench: "playlist", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "gapless", bench: "gapless", sets: &["core"], limit: 90, env: &[], linux: true, kind: Kind::Plain },
    // Its unplayable track is a 5.1 AAC file: the one Windows' ffmpeg (9) writes is one Symphonia
    // refuses, the one Ubuntu 24.04's (6.1) writes it decodes, so on Linux the album has nothing to
    // skip. `skip-damaged` covers the same path on both.
    Mode { name: "skip", bench: "skip", sets: &["core"], limit: 90, env: &[], linux: false, kind: Kind::Plain },
    Mode { name: "skip-damaged", bench: "skip", sets: &["core"], limit: 90, env: &[("MP_BENCH_SKIP_ALBUM", "Damaged")], linux: true, kind: Kind::Plain },
    Mode { name: "theme", bench: "theme", sets: &["core"], limit: 90, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "session", bench: "session-save", sets: &["core", "formats"], limit: 120, env: &[], linux: true, kind: Kind::Session },
    Mode { name: "guard", bench: "guard", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Guard },
    Mode { name: "idle-paused", bench: "idle-paused", sets: &["core"], limit: 90, env: &[("MP_BENCH_IDLE_SECS", "10")], linux: true, kind: Kind::Plain },
    Mode { name: "idle-stopped", bench: "idle-stopped", sets: &["core"], limit: 90, env: &[("MP_BENCH_IDLE_SECS", "10")], linux: true, kind: Kind::Plain },
    Mode { name: "phase4", bench: "phase4", sets: &["core"], limit: 240, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "phase5", bench: "phase5", sets: &["core"], limit: 240, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "phase5-open", bench: "phase5", sets: &["core"], limit: 120, env: &[("MP_BENCH_PART", "open")], linux: true, kind: Kind::Plain },
    Mode { name: "cleanup", bench: "cleanup", sets: &["core"], limit: 240, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "settings", bench: "settings", sets: &["core"], limit: 240, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "autoscroll", bench: "autoscroll", sets: &["core"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "tray", bench: "tray", sets: &["core"], limit: 120, env: &[("MP_BENCH_TRAY_SECS", "8"), ("MP_BENCH_IDLE_SECS", "8")], linux: false, kind: Kind::Plain },
    Mode { name: "about", bench: "about", sets: &["core"], limit: 180, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "details", bench: "details", sets: &["formats"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "details-off", bench: "details", sets: &["formats"], limit: 300, env: &[("MP_BENCH_DETAILS", "off")], linux: true, kind: Kind::Plain },
    Mode { name: "visualizer", bench: "visualizer", sets: &["viz", "words"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    // Writes a .lrc, a .txt and a tag into its own copy of the core set (marked for the editor).
    Mode { name: "lyrics", bench: "lyrics", sets: &["core"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    // The interface size, then the next start on the same data folder keeping it.
    Mode { name: "zoom", bench: "zoom", sets: &["core"], limit: 180, env: &[], linux: true, kind: Kind::Zoom },
    Mode { name: "viz-idle", bench: "viz-idle", sets: &["viz"], limit: 120, env: &[("MP_BENCH_VIZ_SECS", "6"), ("MP_BENCH_IDLE_SECS", "10")], linux: true, kind: Kind::Plain },
    Mode { name: "playback-cost", bench: "playback-cost", sets: &["viz"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "viz-styles", bench: "viz-styles", sets: &["viz"], limit: 600, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "viz-cost", bench: "viz-cost", sets: &["viz"], limit: 400, env: &[("MP_BENCH_VIZ_SECS", "2")], linux: true, kind: Kind::Plain },
    Mode { name: "viz-look", bench: "viz-look", sets: &["viz"], limit: 600, env: &[], linux: true, kind: Kind::Plain },
    // Its Navidrome stand-in runs inside the app (bench/server.rs), on 127.0.0.1.
    Mode { name: "server", bench: "server", sets: &["formats"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    // Its ListenBrainz stand-in runs inside the app (bench/listenbrainz.rs), on 127.0.0.1, reached through
    // the card's own server address.
    Mode { name: "listenbrainz", bench: "listenbrainz", sets: &["core"], limit: 180, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "metadata", bench: "metadata", sets: &["meta"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "home", bench: "home", sets: &["messy"], limit: 180, env: &[], linux: true, kind: Kind::Plain },
    // The hero's play button and strip, the shelf editor's rows, grid shelves at several sizes.
    Mode { name: "home-shelves", bench: "home-shelves", sets: &["core"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "library", bench: "library", sets: &["messy"], limit: 240, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "editing", bench: "editing", sets: &["edit"], limit: 400, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "smart", bench: "smart", sets: &["messy120"], limit: 400, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "eq", bench: "eq", sets: &["messy"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "eq-paste", bench: "eq-paste", sets: &["messy"], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    // Linux: moves PipeWire's default between the headless session's two null sinks (only
    // those; elsewhere it checks the start and changes nothing). On Windows it says so and ends.
    Mode { name: "output-default", bench: "output-default", sets: &["core"], limit: 90, env: &[], linux: true, kind: Kind::Plain },
    // The pointer resting on a track row's play button, at whole and fractional scales.
    Mode { name: "row-hover", bench: "row-hover", sets: &["core"], limit: 90, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "row-hover-125", bench: "row-hover", sets: &["core"], limit: 90, env: &[("SLINT_SCALE_FACTOR", "1.25")], linux: true, kind: Kind::Plain },
    Mode { name: "row-hover-150", bench: "row-hover", sets: &["core"], limit: 90, env: &[("SLINT_SCALE_FACTOR", "1.5")], linux: true, kind: Kind::Plain },
    // And at the interface sizes' ends (zoom.rs), with the frameless window's edge line.
    Mode { name: "row-hover-z70", bench: "row-hover", sets: &["core"], limit: 90, env: &[("MP_BENCH_ZOOM", "70")], linux: true, kind: Kind::Plain },
    Mode { name: "row-hover-z200", bench: "row-hover", sets: &["core"], limit: 90, env: &[("MP_BENCH_ZOOM", "200")], linux: true, kind: Kind::Plain },
    Mode { name: "soak", bench: "soak", sets: &["core"], limit: 400, env: &[("MP_BENCH_SOAK_SECS", "180")], linux: true, kind: Kind::Plain },
    Mode { name: "xfade-rates", bench: "xfade-rates", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Plain },
    // Its own fixtures (ffmpeg) and its own mock of a Navidrome server, in the app: no set needed.
    Mode { name: "streaming", bench: "streaming", sets: &[], limit: 300, env: &[], linux: true, kind: Kind::Plain },
    Mode { name: "bigscan", bench: "bigscan", sets: &[], limit: 1800, env: &[], linux: true, kind: Kind::BigScan },
    Mode { name: "slowscan", bench: "bigscan", sets: &["messy"], limit: 900, env: &[("MP_SLOW_DISK_MS", "8")], linux: true, kind: Kind::SlowScan },
    Mode { name: "data-files", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::DataFiles) },
    Mode { name: "db-garbage", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::DbGarbage) },
    Mode { name: "db-newer", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::DbNewer) },
    Mode { name: "db-flipped", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::DbFlipped) },
    Mode { name: "data-readonly", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: false, kind: Kind::Scenario(Scenario::DataReadOnly) },
    Mode { name: "two-instances", bench: "idle-stopped", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::TwoInstances) },
    Mode { name: "crash", bench: "crash-ui", sets: &["formats"], limit: 120, env: &[], linux: true, kind: Kind::Scenario(Scenario::Crash) },
];

// ---- the mocks ------------------------------------------------------------------------------

/// A tiny HTTP/1.1 server on 127.0.0.1: one request per connection, answered by `handle`, every
/// request logged. `None` from the handler closes the connection without an answer (offline).
struct Mock {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
}

type Answer = Option<(u16, String)>;

fn serve(handle: impl Fn(&str, &str, &HashMap<String, String>) -> Answer + Send + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port for the mock");
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
            if let Some((method, path, form)) = read_request(&conn) {
                l.lock().unwrap().push(format!("{method} {path} {form:?}"));
                if let Some((status, body)) = handle(&method, &path, &form) {
                    let resp = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let mut c = &conn;
                    let _ = c.write_all(resp.as_bytes());
                }
            }
        }
    });
    Mock { url, log }
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

fn form(s: &str) -> HashMap<String, String> {
    s.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect()
}

fn read_request(conn: &TcpStream) -> Option<(String, String, HashMap<String, String>)> {
    let mut r = BufReader::new(conn);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        if h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut f = form(query);
    f.extend(form(&String::from_utf8_lossy(&body)));
    Some((method, path.to_owned(), f))
}

fn record(id: i64, title: &str, synced: Option<&str>, plain: Option<&str>, instrumental: bool) -> String {
    let s = |v: Option<&str>| v.map_or("null".to_owned(), |t| format!("{:?}", t));
    format!(
        r#"{{"id":{id},"trackName":{title:?},"artistName":"Fixture Voices","albumName":"Words","duration":8.0,"instrumental":{instrumental},"plainLyrics":{},"syncedLyrics":{}}}"#,
        s(plain),
        s(synced)
    )
}

/// LRCLIB as `bench/visualizer.rs` expects it for the "Words" album: each title names its answer.
fn lrclib() -> Mock {
    const SYNCED: &str = "[00:00.50]The road runs on\n[00:01.50]lights in the rain\n[00:02.50]no one awake\n[00:03.50]but us\n[00:04.50]and the radio";
    serve(|_, path, q| {
        let title = q.get("track_name").cloned().unwrap_or_default();
        let found = |r: String| Some((200, r));
        let none = || Some((404, r#"{"code":404,"name":"TrackNotFound","message":"Failed to find specified track"}"#.to_owned()));
        if path == "/api/search" {
            return match title.as_str() {
                "Night Drive" => Some((200, format!("[{}]", record(101, "Night Drive", Some(SYNCED), Some("The road runs on"), false)))),
                _ => Some((200, "[]".into())),
            };
        }
        match title.as_str() {
            "Night Drive" => found(record(101, "Night Drive", Some(SYNCED), Some("The road runs on"), false)),
            "Paper Lantern" => found(record(102, "Paper Lantern", None, Some("A paper lantern\nfloats up the hill\nand the hill lets it go"), false)),
            "Interlude" => found(record(103, "Interlude", None, None, true)),
            "Static" => Some((500, r#"{"code":500,"name":"InternalServerError"}"#.into())),
            "Dropout" => None,
            "Skip Four" => found(record(104, "Skip Four", Some(SYNCED), None, false)),
            _ => none(),
        }
    })
}

/// Last.fm's API for a connected session: now playing and scrobbles accepted.
fn lastfm() -> Mock {
    let m = serve(|_, _, f| {
        let method = f.get("method").map(String::as_str).unwrap_or("");
        Some(match method {
            "track.updateNowPlaying" => (200, r##"{"nowplaying":{"ignoredMessage":{"code":"0","#text":""}}}"##.into()),
            "track.scrobble" => (200, r##"{"scrobbles":{"@attr":{"accepted":1,"ignored":0},"scrobble":{"ignoredMessage":{"code":"0","#text":""}}}}"##.into()),
            _ => (200, r#"{"error":10,"message":"Invalid API key (mock)"}"#.into()),
        })
    });
    Mock { url: format!("{}/2.0/", m.url), log: m.log }
}

// ---- files ----------------------------------------------------------------------------------

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    std::fs::create_dir_all(to)?;
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            n += copy_tree(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), &target)?;
            n += 1;
        }
    }
    Ok(n)
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn in_temp(p: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = std::fs::canonicalize(&temp).unwrap_or(temp);
    p.ancestors().find(|a| a.exists()).and_then(|a| std::fs::canonicalize(a).ok()).is_some_and(|a| a.starts_with(&temp))
}

// ---- running --------------------------------------------------------------------------------

#[derive(Clone)]
struct Outcome {
    name: String,
    verdict: String,
    fails: usize,
    secs: f64,
    note: String,
}

#[derive(Clone)]
struct Run<'a> {
    exe: &'a Path,
    dir: PathBuf,
    music: PathBuf,
    data: PathBuf,
    out: PathBuf,
    lrclib: String,
    lastfm: String,
}

impl Run<'_> {
    /// One launch of the app; its exit code, or `None` when it had to be stopped.
    fn launch(&self, bench: &str, env: &[(&str, &str)], limit: u64, extra: &[(&str, &Path)]) -> (Option<i32>, f64) {
        let t0 = Instant::now();
        let Ok(child) = self.command(bench, env, extra, "").spawn() else { return (Some(-1), 0.0) };
        wait(child, limit, t0)
    }

    /// `launch`, its stderr kept apart (`stderr-<bench><tag>.txt`).
    fn launch_tagged(&self, bench: &str, env: &[(&str, &str)], limit: u64, extra: &[(&str, &Path)], tag: &str) -> (Option<i32>, f64) {
        let t0 = Instant::now();
        let Ok(child) = self.command(bench, env, extra, tag).spawn() else { return (Some(-1), 0.0) };
        wait(child, limit, t0)
    }

    /// The app's command line, its stderr to `stderr-<bench><tag>.txt`.
    fn command(&self, bench: &str, env: &[(&str, &str)], extra: &[(&str, &Path)], tag: &str) -> Command {
        let mut cmd = Command::new(self.exe);
        cmd.env("MP_BENCH", bench)
            .env("MP_BENCH_OUT", &self.out)
            .env("MP_DATA_DIR", &self.data)
            .env("MP_MUSIC", &self.music)
            .env("MP_VOLUME", "0")
            .env("MP_WINDOW", "secondary")
            .env("MP_LRCLIB_API", &self.lrclib)
            .env("MP_LASTFM_API", &self.lastfm)
            .env("RUST_BACKTRACE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.dir.join(format!("stderr-{bench}{tag}.txt"))).map(Stdio::from).unwrap_or(Stdio::null()));
        for (k, v) in env {
            cmd.env(k, v);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd
    }
}

/// Wait for a launched app; its exit code, or `None` when it outlived `limit` and was stopped
/// (this child only, by its handle).
fn wait(mut child: std::process::Child, limit: u64, t0: Instant) -> (Option<i32>, f64) {
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return (Some(s.code().unwrap_or(-2)), t0.elapsed().as_secs_f64());
        }
        if t0.elapsed() > Duration::from_secs(limit) {
            let _ = child.kill();
            let _ = child.wait();
            return (None, t0.elapsed().as_secs_f64());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Fill `bytes` from `at` on, every 7th byte for `len` bytes, flipped.
fn flip(bytes: &mut [u8], at: usize, len: usize) {
    for b in bytes.iter_mut().skip(at).take(len).step_by(7) {
        *b ^= 0xFF;
    }
}

/// A scenario of the app's own files misbehaving: set up, launched, checked here; the checks
/// go to `bench-<scenario>.txt` as `[ok]` / `[FAIL]` lines like the app's own.
fn scenario(s: Scenario, run: &Run, m: &Mode) -> (Option<i32>, f64) {
    let data = &run.data;
    let log = || std::fs::read_to_string(data.join("marquee-player.log")).unwrap_or_default();
    let stderr = |tag: &str| std::fs::read_to_string(run.dir.join(format!("stderr-idle-stopped{tag}.txt"))).unwrap_or_default();
    let idle = [("MP_BENCH_IDLE_SECS", "2")];
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    let mut check = |what: &str, ok: bool, detail: String| checks.push((what.to_owned(), ok, detail));
    let mut secs = 0.0;
    match s {
        Scenario::DataFiles => {
            let folder = run.music.display().to_string().replace('\\', "\\\\");
            let _ = std::fs::write(data.join("settings.json"), format!(r#"{{ "folders": ["{folder}"], "volume": "loud", "exclusive": true, "crossfade": 3 }}"#));
            let _ = std::fs::write(data.join("lastfm.json"), "{ not json");
            let _ = std::fs::write(data.join("scrobbles.json"), r#"[{"artist": "A"#);
            let _ = std::fs::write(data.join("theme.toml"), "[colors\naccent = ");
            let _ = std::fs::create_dir_all(data.join("edits").join("inflight"));
            let _ = std::fs::write(data.join("edits").join("inflight").join("junk.json"), "garbage");
            let (c, t) = run.launch("idle-stopped", &idle, m.limit, &[]);
            secs += t;
            let saved = std::fs::read_to_string(data.join("settings.json")).unwrap_or_default();
            let l = log();
            check("the app starts and runs as usual", c == Some(0), format!("exit {c:?}"));
            check("settings.json: the readable settings are kept (the folder, exclusive mode)", saved.contains(&folder) && saved.contains(r#""exclusive": true"#), saved.lines().take(4).collect::<Vec<_>>().join(" "));
            check("settings.json: the old file is kept as settings.json.bad", data.join("settings.json.bad").exists(), String::new());
            check("settings.json: what was reset is logged (and shown in the pill)", l.contains("volume") && l.contains("settings.json"), String::new());
            check("lastfm.json and scrobbles.json: kept as .bad before anything replaces them", data.join("lastfm.json.bad").exists() && data.join("scrobbles.json.bad").exists(), String::new());
            check("theme.toml: the built-in theme is used, the file left as it is", std::fs::read_to_string(data.join("theme.toml")).unwrap_or_default() == "[colors\naccent = " && l.contains("theme.toml"), String::new());
            check("a junk record in edits/inflight is dropped", !data.join("edits").join("inflight").join("junk.json").exists(), String::new());
        }
        Scenario::DbGarbage => {
            let junk = vec![0x5au8; 8192];
            let _ = std::fs::write(data.join("library.db"), &junk);
            let (c, t) = run.launch("idle-stopped", &idle, m.limit, &[]);
            secs += t;
            let e = stderr("");
            check("refused with a message, not a crash", matches!(c, Some(1) | Some(3)) && e.contains("damaged and cannot be read"), format!("exit {c:?}"));
            check("the message says what to do", e.contains("Start with a new library?") && e.contains("music files are not touched"), String::new());
            check("the file is left as it was", std::fs::read(data.join("library.db")).unwrap_or_default() == junk, String::new());
            let (c, t) = run.launch("idle-stopped", &[("MP_BENCH_IDLE_SECS", "2"), ("MP_BENCH_ANSWER", "yes")], m.limit, &[]);
            secs += t;
            let aside = std::fs::read_dir(data).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("library.damaged-")).count()).unwrap_or(0);
            check("\"start with a new library\": it starts, scans, and keeps the damaged file aside", c == Some(0) && aside == 1, format!("exit {c:?}, {aside} kept aside"));
        }
        Scenario::DbNewer => {
            let (c, t) = run.launch("idle-stopped", &idle, m.limit, &[]);
            secs += t;
            check("a first start makes the library", c == Some(0), format!("exit {c:?}"));
            let mut db = std::fs::read(data.join("library.db")).unwrap_or_default();
            if db.len() > 64 {
                db[60..64].copy_from_slice(&99u32.to_be_bytes());
                let _ = std::fs::write(data.join("library.db"), &db);
            }
            let (c, t) = run.launch_tagged("idle-stopped", &idle, m.limit, &[], "-2");
            secs += t;
            let e = stderr("-2");
            check("a newer build's library is refused with a message", matches!(c, Some(1) | Some(3)) && e.contains("newer version of Marquee Player"), format!("exit {c:?}"));
            check("and left untouched", std::fs::read(data.join("library.db")).unwrap_or_default() == db, String::new());
        }
        Scenario::DbFlipped => {
            let (c, t) = run.launch("idle-stopped", &idle, m.limit, &[]);
            secs += t;
            check("a first start makes the library", c == Some(0), format!("exit {c:?}"));
            let mut db = std::fs::read(data.join("library.db")).unwrap_or_default();
            let n = db.len();
            // The middle of the file: table and index pages. A third of the way in was one of the
            // schema's own pages from schema 12 on (58 pages, the schema on 1, 24, 25 and 45), and
            // a database whose schema cannot be read does not open at all, which is `db-garbage`'s
            // case, not this one.
            flip(&mut db, n / 2, 16 * 1024);
            let _ = std::fs::write(data.join("library.db"), &db);
            let (c, t) = run.launch_tagged("idle-stopped", &idle, m.limit, &[], "-2");
            secs += t;
            let e = stderr("-2");
            check("a library damaged inside: no crash, no hang", matches!(c, Some(0) | Some(3)), format!("exit {c:?}"));
            check("the user is told the library is damaged, and the app closes", e.contains("found its library database damaged"), e.lines().filter(|l| l.contains("malformed") || l.contains("damaged")).take(2).collect::<Vec<_>>().join(" | "));
            let (c, t) = run.launch_tagged("idle-stopped", &idle, m.limit, &[], "-3");
            secs += t;
            check("the next start offers a new library", matches!(c, Some(1) | Some(3)) && stderr("-3").contains("Start with a new library?"), format!("exit {c:?}"));
            let (c, t) = run.launch_tagged("idle-stopped", &[("MP_BENCH_IDLE_SECS", "2"), ("MP_BENCH_ANSWER", "yes")], m.limit, &[], "-4");
            secs += t;
            check("and starts with one when asked, the damaged file kept", c == Some(0) && !library_marker(data), format!("exit {c:?}"));
        }
        Scenario::DataReadOnly => {
            let (c, t) = run.launch("idle-stopped", &idle, m.limit, &[]);
            secs += t;
            check("a first start makes the data folder", c == Some(0), format!("exit {c:?}"));
            let user = std::env::var("USERNAME").unwrap_or_default();
            let deny = Command::new("icacls").arg(data).args(["/deny", &format!("{user}:(OI)(CI)(W,D,DC)"), "/Q"]).stdout(Stdio::null()).status().is_ok_and(|s| s.success());
            check("the data folder made unwritable (icacls deny write)", deny, String::new());
            let (c, t) = run.launch_tagged("idle-stopped", &idle, m.limit, &[], "-2");
            secs += t;
            let e = stderr("-2");
            let _ = Command::new("icacls").arg(data).args(["/remove:d", &user, "/T", "/Q"]).stdout(Stdio::null()).status();
            check("refused at start with a message naming the folder and what to do", matches!(c, Some(1) | Some(3)) && e.contains("cannot write to the folder"), format!("exit {c:?}: {}", e.lines().filter(|l| l.contains("ERROR")).take(1).collect::<String>()));
        }
        Scenario::Crash => {
            let (c, t) = run.launch_tagged("crash-ui", &[], m.limit, &[], "");
            secs += t;
            let e = std::fs::read_to_string(run.dir.join("stderr-crash-ui.txt")).unwrap_or_default();
            let l = log();
            check("a panic on the UI thread ends the process (no hang)", c == Some(101), format!("exit {c:?}"));
            check("the log has it, with its thread and a backtrace", l.contains("FATAL: code panicked in thread 'main'") && l.contains("backtrace:"), String::new());
            check("the user is told what happened, where the log is and how to report it", e.contains("Something went wrong inside Marquee Player") && e.contains("marquee-player.log") && e.contains("Copy diagnostics"), String::new());
            let (c, t) = run.launch_tagged("idle-stopped", &idle, m.limit, &[], "-after");
            secs += t;
            let prev = std::fs::read_to_string(data.join("marquee-player.previous.log")).unwrap_or_default();
            check("the next start runs, and keeps the crashed run's log as the previous one", c == Some(0) && prev.contains("FATAL: code panicked"), format!("exit {c:?}"));
            let (c, t) = run.launch_tagged("crash-worker", &[], m.limit, &[], "-worker");
            secs += t;
            let e = std::fs::read_to_string(run.dir.join("stderr-crash-worker-worker.txt")).unwrap_or_default();
            check("a panic on another thread: told, and the app closes the normal way", matches!(c, Some(0) | Some(3)) && e.contains("mp-bench-crash") && e.contains("has to close"), format!("exit {c:?}"));
        }
        Scenario::TwoInstances => {
            let t0 = Instant::now();
            let a = run.command("idle-stopped", &[("MP_BENCH_IDLE_SECS", "8")], &[], "-a").spawn();
            std::thread::sleep(Duration::from_millis(1500));
            let b = run.command("idle-stopped", &[("MP_BENCH_IDLE_SECS", "8")], &[], "-b").spawn();
            let (cb, tb) = b.map_or((Some(-1), 0.0), |b| wait(b, 30, Instant::now()));
            let (ca, _) = a.map_or((Some(-1), 0.0), |a| wait(a, m.limit, t0));
            secs = t0.elapsed().as_secs_f64();
            check("the second launch hands over and leaves at once", cb == Some(0) && tb < 10.0, format!("exit {cb:?} after {tb:.1} s"));
            check("the first runs to its end as usual", ca == Some(0), format!("exit {ca:?}"));
            let l = log();
            check("the running copy's log is its own (the second did not restart it)", l.contains("on Windows") || l.contains("on Linux") || l.lines().count() > 5, format!("{} lines", l.lines().count()));
        }
    }
    let ok = checks.iter().all(|c| c.1);
    let text: String = checks.iter().map(|(w, ok, d)| format!("  [{}] {w}: {d}\n", if *ok { "ok" } else { "FAIL" })).collect();
    let _ = std::fs::write(run.out.join(format!("bench-{}.txt", m.name)), text);
    (Some(if ok { 0 } else { 3 }), secs)
}

/// The snapshots of a run quit mid-scan, kept apart from the next run's: `quit-<name>.png`.
fn keep_as_quit(out: &Path) {
    for e in std::fs::read_dir(out).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("bigscan-") && name.ends_with(".png") {
            let _ = std::fs::rename(e.path(), out.join(format!("quit-{name}")));
        }
    }
}

/// `[FAIL]` and `FAIL` lines in the mode's reports.
fn failed_lines(out: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let Ok(rd) = std::fs::read_dir(out) else { return lines };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "txt") {
            for l in std::fs::read_to_string(&p).unwrap_or_default().lines() {
                if l.contains("[FAIL]") || l.trim_start().starts_with("FAIL") || l.contains("-> FAIL") || l.contains("FAIL:") {
                    lines.push(l.trim().to_owned());
                }
            }
        }
    }
    lines
}

fn run_mode(exe: &Path, fixtures: &Path, root: &Path, m: &Mode, keep: bool) -> Outcome {
    let dir = root.join(m.name);
    let _ = std::fs::remove_dir_all(&dir);
    let music = dir.join("music");
    let data = dir.join("data");
    let out = dir.join("out");
    for d in [&music, &data, &out] {
        std::fs::create_dir_all(d).unwrap();
    }
    for set in m.sets {
        if let Err(e) = copy_tree(&fixtures.join(set), &music.join(set)) {
            return Outcome { name: m.name.into(), verdict: "ERROR".into(), fails: 0, secs: 0.0, note: format!("copying {set}: {e}") };
        }
    }
    // A connected Last.fm session (against the mock).
    let _ = std::fs::write(data.join("lastfm.json"), r#"{"api_key":"benchkey0123456789","api_secret":"benchsecret","session_key":"bench-sk","username":"bench"}"#);
    let lrc = lrclib();
    let lfm = lastfm();
    // One set: the library is that set's folder (the metadata and editing benches check that it is
    // their fixtures, marked); several: the folder holding them.
    let library = if m.sets.len() == 1 { music.join(m.sets[0]) } else { music.clone() };
    let run = Run { exe, dir: dir.clone(), music: library, data, out: out.clone(), lrclib: lrc.url.clone(), lastfm: lfm.url.clone() };
    let mut note = String::new();
    let (code, secs) = match m.kind {
        Kind::Plain => run.launch(m.bench, m.env, m.limit, &[]),
        Kind::Scenario(s) => scenario(s, &run, m),
        Kind::Session => {
            let (c1, s1) = run.launch("session-save", m.env, m.limit, &[]);
            if c1 != Some(0) {
                note = "session-save did not pass".into();
                (c1, s1)
            } else {
                // Move the folder of the track with the history between the runs: its plays,
                // favourite and playlist entry must follow it.
                let report = std::fs::read_to_string(out.join("bench-session-save.txt")).unwrap_or_default();
                let track = report.lines().find_map(|l| l.strip_prefix("history on ").and_then(|l| l.split_once(") at ")).map(|(_, p)| PathBuf::from(p.trim())));
                if let Some(album) = track.as_deref().and_then(Path::parent) {
                    let moved = album.with_file_name(format!("{} (moved)", album.file_name().unwrap_or_default().to_string_lossy()));
                    if let Err(e) = std::fs::rename(album, &moved) {
                        note = format!("could not move {}: {e}", album.display());
                    }
                } else {
                    note = "session-save named no track".into();
                }
                // A second folder, outside the first, to add and remove while a scan runs.
                let extra = dir.join("extra");
                let _ = std::fs::rename(music.join("formats"), extra.join("formats")).or_else(|_| {
                    std::fs::create_dir_all(&extra)?;
                    std::fs::rename(music.join("formats"), extra.join("formats"))
                });
                let (c2, s2) = run.launch("session-check", &[("MP_BENCH_MOVED", " (moved)")], m.limit, &[("MP_BENCH_EXTRA", &extra.join("formats"))]);
                (c2, s1 + s2)
            }
        }
        Kind::Zoom => {
            let (c1, s1) = run.launch("zoom", m.env, m.limit, &[]);
            if c1 != Some(0) {
                note = "zoom did not pass".into();
                (c1, s1)
            } else {
                let (c2, s2) = run.launch("zoom-check", m.env, m.limit, &[]);
                (c2, s1 + s2)
            }
        }
        Kind::BigScan => {
            // Scanned where it is (read only), never copied: tens of thousands of files.
            let big = fixtures.join("big");
            let run = Run { music: big, ..run.clone() };
            let (c1, s1) = run.launch("bigscan", &[("MP_BENCH_QUIT_AFTER", "3")], m.limit, &[]);
            let _ = std::fs::rename(out.join("bench-bigscan.txt"), out.join("bench-bigscan-quit.txt"));
            keep_as_quit(&out);
            if c1 != Some(0) {
                note = format!("quitting mid-scan: exit {c1:?} after {s1:.1} s");
                (c1, s1)
            } else {
                // Quit 3 s in (mid-scan, even with the files in the system's cache): the process should be
                // gone promptly, not after the scan.
                if s1 > 20.0 {
                    note = format!("quitting mid-scan took the process {s1:.1} s to end");
                }
                let (c2, s2) = run.launch("bigscan", &[], m.limit, &[]);
                (if s1 > 20.0 && c2 == Some(0) { Some(3) } else { c2 }, s1 + s2)
            }
        }
        Kind::SlowScan => {
            let quit: Vec<(&str, &str)> = m.env.iter().copied().chain([("MP_BENCH_QUIT_AT", "0.4")]).collect();
            let (c1, s1) = run.launch("bigscan", &quit, m.limit, &[]);
            let _ = std::fs::rename(out.join("bench-bigscan.txt"), out.join("bench-bigscan-quit.txt"));
            keep_as_quit(&out);
            if c1 != Some(0) {
                note = format!("quitting 40% through: exit {c1:?} after {s1:.1} s");
                (c1, s1)
            } else {
                let (c2, s2) = run.launch("bigscan", m.env, m.limit, &[]);
                (c2, s1 + s2)
            }
        }
        Kind::Guard => {
            // The first run scans everything and plays nothing.
            let (c1, s1) = run.launch("idle-stopped", &[("MP_BENCH_IDLE_SECS", "1")], m.limit, &[]);
            if c1 != Some(0) {
                note = "the first scan did not finish".into();
                (c1, s1)
            } else {
                let mut all = Vec::new();
                files(&music, &mut all);
                all.sort();
                for (i, f) in all.iter().enumerate() {
                    if i % 20 != 0 {
                        let _ = std::fs::remove_file(f);
                    }
                }
                let (c2, s2) = run.launch(m.bench, m.env, m.limit, &[]);
                (c2, s1 + s2)
            }
        }
    };
    let _ = std::fs::write(dir.join("lrclib-mock.log"), lrc.log.lock().unwrap().join("\n"));
    let _ = std::fs::write(dir.join("lastfm-mock.log"), lfm.log.lock().unwrap().join("\n"));
    let fails = failed_lines(&out);
    let log = std::fs::read_to_string(run.data.join("marquee-player.log")).unwrap_or_default();
    // (The crash scenario panics on purpose: its own checks say whether that went as it should.)
    let panicked = m.kind != Kind::Scenario(Scenario::Crash) && log.lines().any(|l| l.contains("panicked"));
    let verdict = match code {
        Some(0) if fails.is_empty() && !panicked => "PASS",
        Some(0) if panicked => "PANIC",
        Some(0) | Some(3) => "FAIL",
        Some(101) => "PANIC",
        None => "HANG",
        Some(_) => "CRASH",
    };
    if note.is_empty() {
        note = match code {
            Some(c) if c != 0 && c != 3 => format!("exit code {c}"),
            None => format!("stopped after {} s", m.limit),
            _ => fails.first().cloned().unwrap_or_default(),
        };
    }
    if !keep {
        let _ = std::fs::remove_dir_all(&music);
    }
    let _ = std::fs::write(dir.join("failed.txt"), fails.join("\n"));
    Outcome { name: m.name.into(), verdict: verdict.into(), fails: fails.len(), secs, note }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let me = std::env::current_exe().unwrap();
    let mut exe = me.with_file_name(format!("MarqueePlayer{}", std::env::consts::EXE_SUFFIX));
    let mut fixtures = std::env::temp_dir().join("mp-bench-fixtures");
    let mut root = std::env::temp_dir().join("mp-bench-runs");
    let (mut only, mut skip, mut keep) = (Vec::<String>::new(), Vec::<String>::new(), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            // Everything but the modes that are long by design: for a run while working.
            "--quick" => skip.extend(LONG.iter().map(|s| s.to_string())),
            "--exe" => exe = PathBuf::from(args.next().expect("--exe <path>")),
            "--fixtures" => fixtures = PathBuf::from(args.next().expect("--fixtures <dir>")),
            "--out" => root = PathBuf::from(args.next().expect("--out <dir>")),
            "--only" => only = args.next().expect("--only a,b").split(',').map(str::to_owned).collect(),
            "--skip" => skip.extend(args.next().expect("--skip a,b").split(',').map(str::to_owned)),
            "--keep" => keep = true,
            "--list" => {
                for m in MODES {
                    let long = if LONG.contains(&m.name) { " (long: not in --quick)" } else { "" };
                    println!("{}{}{long}", m.name, if m.linux { "" } else { " (Windows only)" });
                }
                return;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    // A name that is no mode's (a list PowerShell joined with spaces when it was not quoted, a
    // misspelling) would otherwise run nothing and pass.
    if let Some(unknown) = only.iter().chain(&skip).find(|n| !MODES.iter().any(|m| m.name == n.as_str())) {
        eprintln!("no bench mode is called \"{unknown}\" (--list prints them; in PowerShell, quote a list: '--only' 'a,b')");
        std::process::exit(2);
    }
    assert!(exe.exists(), "{} not found: build it with cargo build --release -p marquee-player --features bench", exe.display());
    assert!(in_temp(&root), "{} is not in the temporary folder: refused", root.display());
    if !fixtures.join(".mp-bench-fixtures").exists() {
        let tool = me.with_file_name("examples").join(format!("bench_fixtures{}", std::env::consts::EXE_SUFFIX));
        println!("making the fixtures in {} ...", fixtures.display());
        let ok = Command::new(&tool).arg(&fixtures).status().is_ok_and(|s| s.success());
        assert!(ok, "{} failed (cargo build --release -p library --examples)", tool.display());
    }
    // The visualizer's set as made before it had its bright and monochrome covers ("Paper",
    // "Graphite"): made again, in place of the old one (inside the fixtures folder only).
    let viz = fixtures.join("viz");
    if viz.exists() && !viz.join("Fixture Covers").join("Graphite").exists() {
        let tool = me.with_file_name("examples").join(format!("viz_fixtures{}", std::env::consts::EXE_SUFFIX));
        let fresh = fixtures.join("viz.new");
        let _ = std::fs::remove_dir_all(&fresh);
        println!("making the visualizer's fixtures again in {} (its newer covers) ...", viz.display());
        let ok = Command::new(&tool).arg(&fresh).stdout(Stdio::null()).status().is_ok_and(|s| s.success());
        assert!(ok, "{} failed (cargo build --release -p library --examples)", tool.display());
        std::fs::remove_dir_all(&viz).expect("the old visualizer fixtures");
        std::fs::rename(&fresh, &viz).expect("the new visualizer fixtures in place");
    }
    // The large library (about 30,000 tracks), made the first time a mode needs it, kept.
    let wants_big = MODES.iter().any(|m| m.kind == Kind::BigScan && (only.is_empty() || only.iter().any(|o| o == m.name)) && !skip.iter().any(|s| s == m.name));
    if wants_big && !fixtures.join("big").exists() {
        let tool = me.with_file_name("examples").join(format!("messy_library{}", std::env::consts::EXE_SUFFIX));
        println!("making the large library in {} (a few minutes, about 600 MB) ...", fixtures.join("big").display());
        let ok = Command::new(&tool).arg(fixtures.join("big")).arg("2600").stdout(Stdio::null()).status().is_ok_and(|s| s.success());
        assert!(ok, "{} failed", tool.display());
    }
    if root.exists() {
        let _ = std::fs::remove_dir_all(&root);
    }
    std::fs::create_dir_all(&root).unwrap();
    let elsewhere = !cfg!(windows);
    let modes: Vec<&Mode> = MODES
        .iter()
        .filter(|m| only.is_empty() || only.iter().any(|o| o == m.name))
        .filter(|m| !skip.iter().any(|s| s == m.name))
        .collect();
    let t0 = Instant::now();
    let mut results = Vec::new();
    for m in modes {
        if elsewhere && !m.linux {
            results.push(Outcome { name: m.name.into(), verdict: "n/a".into(), fails: 0, secs: 0.0, note: "Windows only".into() });
            continue;
        }
        print!("{:<14} ", m.name);
        let _ = std::io::stdout().flush();
        let o = run_mode(&exe, &fixtures, &root, m, keep);
        println!("{:<6} {:>6.1} s  {}", o.verdict, o.secs, o.note);
        results.push(o);
    }
    let mut table = String::new();
    table.push_str(&format!("\n{:<14} {:<6} {:>6} {:>8}  {}\n", "mode", "result", "fails", "seconds", "first failure / note"));
    for o in &results {
        table.push_str(&format!("{:<14} {:<6} {:>6} {:>8.1}  {}\n", o.name, o.verdict, o.fails, o.secs, o.note.chars().take(110).collect::<String>()));
    }
    let passed = results.iter().filter(|o| o.verdict == "PASS").count();
    let counted = results.iter().filter(|o| o.verdict != "n/a").count();
    table.push_str(&format!("\n{passed} of {counted} passed in {:.0} s; reports in {}\n", t0.elapsed().as_secs_f64(), root.display()));
    print!("{table}");
    let _ = std::fs::write(root.join("table.txt"), &table);
    std::process::exit(if passed == counted { 0 } else { 1 });
}

/// Whether the app's "database found damaged" marker is still beside the library.
fn library_marker(data: &Path) -> bool {
    data.join("library.db.damaged").exists()
}
