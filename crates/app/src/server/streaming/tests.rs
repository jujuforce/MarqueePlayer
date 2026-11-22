//! The downloader and the stream cache against a local mock of the server (`streammock`), with
//! the real engine on a device that writes nowhere where playing matters. What must hold: each
//! format starts well before its download ends on a slow link (Ogg and MP4 with `moov` at the end
//! through a range for their end); a seek ahead is a range request; a stall and a dropped
//! connection go on from where they stopped; a missing file fails at once with words for it; a
//! cached file plays with no request at all; a partial file after a crash is completed, never
//! played as whole; two server tracks are gapless; eviction keeps the cap and the kept tracks;
//! skipping and quitting leave no thread or connection behind. Needs ffmpeg (skips without).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use player_core::output::Device;
use player_core::{Event, Player};

use super::*;
use crate::server::streammock::{self, FORMATS, Mock};

#[test]
fn stepping_quality_reaches_each_preset_and_stops_at_the_ends() {
    let mut quality = None;
    for kbps in [320, 192, 128, 96, 96] {
        quality = quality_step(quality, -1);
        assert_eq!(quality, Some(kbps));
    }
    assert_eq!(quality_step(Some(96), 1), Some(128));
    assert_eq!(quality_step(Some(320), 1), None);
    assert_eq!(quality_step(None, 1), None);
    assert_eq!(quality_step(Some(123), 0), None);
}

/// A device that takes audio at about ten times real time and keeps nothing.
#[derive(Clone, Default)]
struct Null {
    frames: Arc<AtomicUsize>,
}

impl Device for Null {
    fn open(&mut self, _rate: u32) -> Result<(), String> {
        Ok(())
    }
    fn wait(&mut self, _timeout: Duration) {
        std::thread::sleep(Duration::from_millis(1));
    }
    fn available(&mut self) -> Result<usize, String> {
        Ok(441)
    }
    fn queued(&mut self) -> usize {
        0
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        self.frames.fetch_add(samples.len() / 2, Ordering::Relaxed);
        Ok(())
    }
    fn start(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn flush(&mut self) {}
    fn realtime(&self) -> bool {
        false
    }
}

fn player() -> (Player, Receiver<Event>) {
    Player::with_device(|| Box::new(Null::default()))
}

fn dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join("mp-stream-tests").join(format!("{}-{}-{name}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn fixtures(seconds: u32) -> Option<Vec<PathBuf>> {
    if !streammock::have_ffmpeg() {
        eprintln!("ffmpeg not on PATH: skipping");
        return None;
    }
    Some(streammock::fixtures(&std::env::temp_dir().join("mp-stream-fixtures"), seconds))
}

fn info(id: &str, bytes: &[u8], suffix: &str) -> StreamInfo {
    StreamInfo { song_id: id.to_owned(), suffix: Some(suffix.to_owned()), content_type: None, size: bytes.len() as i64, ..Default::default() }
}

fn data_path(dir: &Path, info: &StreamInfo) -> PathBuf {
    CachedFile::new(dir, info, None).path
}

/// A streamer on a fresh folder, signed in to the mock, quick to notice a stall.
fn streamer(mock: &Mock, name: &str) -> Arc<Streamer> {
    let mut cfg = Config::new(dir(name), DEFAULT_CAP);
    cfg.read_timeout_ms = 700;
    cfg.backoff = vec![Duration::from_millis(100), Duration::from_millis(200)];
    cfg.attempts = 4;
    let s = Streamer::new(cfg);
    s.set_client(Some(mock.client()));
    s
}

fn wait_until(limit: Duration, what: &str, mut f: impl FnMut() -> bool) -> Duration {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < limit, "{what}: not within {limit:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    t.elapsed()
}

/// Events until `f` matches one (which is returned), within `limit`.
fn until(rx: &Receiver<Event>, limit: Duration, what: &str, f: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) if f(&e) => return e,
            Ok(Event::Error { message, .. }) => panic!("{what}: engine error {message}"),
            Ok(_) => {}
            Err(_) => panic!("{what}: not within {limit:?}"),
        }
    }
}

fn feed_of(r: &TrackRequest) -> Feed {
    r.feed.clone().expect("a growing request")
}

#[test]
fn stretches_and_gaps() {
    let mut f = Vec::new();
    add_range(&mut f, 10..20);
    add_range(&mut f, 30..40);
    add_range(&mut f, 20..25);
    assert_eq!(f, vec![10..25, 30..40]);
    assert_eq!(gaps(&f, 50), vec![0..10, 25..30, 40..50]);
    // A reader's offset first, from there to the end of its gap.
    assert_eq!(next_stretch(&f, 50, Some(27), 0), Some(27..30));
    // Else on from where the last stretch ended, then round to the start.
    assert_eq!(next_stretch(&f, 50, None, 25), Some(25..30));
    assert_eq!(next_stretch(&f, 50, None, 45), Some(45..50));
    assert_eq!(next_stretch(&f, 50, None, 50), Some(0..10));
    add_range(&mut f, 0..50);
    assert_eq!(next_stretch(&f, 50, None, 0), None);
}

#[test]
fn keys_name_the_song_and_its_size() {
    let i = StreamInfo { song_id: "3f2a9cDe_x-1".into(), suffix: Some("FLAC".into()), content_type: None, size: 1234, ..Default::default() };
    assert_eq!(key(&i), "3f2a9cDe_x-1-1234");
    assert_eq!(extension(&i), "flac");
    // A changed file under the same id is another file.
    assert_ne!(key(&i), key(&StreamInfo { size: 1235, ..i.clone() }));
    // Ids that cannot be file names are hashed.
    let odd = StreamInfo { song_id: "a/b:c".into(), suffix: Some("../x".into()), ..i };
    assert!(key(&odd).starts_with('x') && !key(&odd).contains(['/', ':']));
    assert_eq!(extension(&odd), "audio");
}

/// The time to first sound of each format on a link that takes ~4 s for the whole file: well
/// under half of it, each format opening as it must (Opus and the MP4 with `moov` at the end
/// through a range for their end).
#[test]
fn each_format_starts_well_before_its_download_ends() {
    let Some(files) = fixtures(20) else { return };
    let mut report = String::new();
    for (i, (name, _, ct)) in FORMATS.iter().enumerate() {
        let bytes = fs::read(&files[i]).unwrap();
        let mock = Mock::start();
        let id = format!("s{i}");
        mock.add(&id, bytes.clone(), ct);
        // About 4 s for the whole file.
        let rate = bytes.len() as u64 / 4;
        mock.set(|b| b.rate = rate);
        let s = streamer(&mock, name);
        let (p, rx) = player();
        let t = Instant::now();
        let req = s.request(1, &info(&id, &bytes, name.rsplit('.').next().unwrap())).unwrap();
        let feed = feed_of(&req);
        p.play(req);
        until(&rx, Duration::from_secs(15), name, |e| matches!(e, Event::TrackStarted { token: 1 }));
        let started = t.elapsed();
        // What had come from the server by then (the folder's sizes lag for a file being written).
        let sent_at_start = mock.sent();
        // Said in full when it does not come: which format, how far the server got, what it
        // was asked and what it is doing.
        let since = Instant::now();
        while !feed.is_complete() {
            assert!(
                since.elapsed() < Duration::from_secs(20),
                "{name}: not whole within 20 s of its first sound ({:.0} ms): the server sent {} of {} bytes at {rate} B/s, was asked from {:?}, is doing {:?}; the reader waits at {:?}",
                started.as_secs_f64() * 1000.0,
                mock.sent(),
                bytes.len(),
                mock.streams().iter().map(|r| r.from).collect::<Vec<_>>(),
                mock.doing(),
                feed.wanted()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let whole = t.elapsed();
        let ranges: Vec<u64> = mock.streams().iter().map(|r| r.from).filter(|&f| f > 0).collect();
        let line = format!(
            "{name}: {} KB at {} KB/s: first sound {:.0} ms, whole {:.0} ms ({:.0}% sent by the start), {} requests, ranges from {ranges:?}\n",
            bytes.len() / 1024,
            rate / 1024,
            started.as_secs_f64() * 1000.0,
            whole.as_secs_f64() * 1000.0,
            100.0 * sent_at_start as f64 / bytes.len() as f64,
            mock.streams().len()
        );
        eprint!("{line}");
        report.push_str(&line);
        assert!(started.as_secs_f64() < whole.as_secs_f64() * 0.4, "{name} started late: {line}");
        if name.ends_with(".opus") || name.contains("moov-end") {
            assert!(!ranges.is_empty(), "{name}: its end was fetched first, by a range");
        }
        // The file on disk is the server's, byte for byte.
        let cached = fs::read(data_path(s.dir(), &info(&id, &bytes, name.rsplit('.').next().unwrap()))).unwrap();
        assert!(cached == bytes, "{name}: the cached file differs");
        p.stop();
        drop(p);
        assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    }
    let _ = fs::write(std::env::temp_dir().join("mp-stream-tests").join("first-sound.txt"), report);
}

#[test]
fn a_seek_ahead_is_a_range_request() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[0]).unwrap();
    let mock = Mock::start();
    mock.add("f", bytes.clone(), "audio/flac");
    // 10 s for the whole file: a seek to 15 s is far past the download.
    mock.set(|b| b.rate = bytes.len() as u64 / 10);
    let s = streamer(&mock, "seek");
    let (p, rx) = player();
    let req = s.request(1, &info("f", &bytes, "flac")).unwrap();
    let feed = feed_of(&req);
    p.play(req);
    until(&rx, Duration::from_secs(10), "start", |e| matches!(e, Event::TrackStarted { token: 1 }));
    let t = Instant::now();
    p.seek(15.0);
    wait_until(Duration::from_secs(6), "playing past 15 s", || p.status().position > 15.2);
    let took = t.elapsed();
    let far: Vec<u64> = mock.streams().iter().map(|r| r.from).filter(|&f| f > bytes.len() as u64 / 2).collect();
    eprintln!("seek to 15 s of 20: playing there after {took:?}; ranges from {far:?}; whole: {}", feed.is_complete());
    assert!(!far.is_empty(), "a range from the seek's place");
    assert!(!feed.is_complete(), "played there before the download got there");
    p.stop();
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn a_stall_and_a_drop_go_on_from_where_they_stopped() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[1]).unwrap();
    let len = bytes.len() as u64;
    for drop in [false, true] {
        let mock = Mock::start();
        mock.add("m", bytes.clone(), "audio/mpeg");
        let at = len / 2 + 777;
        mock.set(|b| {
            b.rate = len / 2;
            if drop {
                b.drop_at = Some(at);
            } else {
                // Until the streamer gives up on the connection (its read timeout), however long
                // the system takes to: the stall always ends in a retry.
                b.stall_at = Some((at, Duration::from_secs(60)));
            }
        });
        let s = streamer(&mock, "stall");
        let req = s.request(1, &info("m", &bytes, "mp3")).unwrap();
        let feed = feed_of(&req);
        // Kept (the current track) though nothing reads it.
        s.keep(&[info("m", &bytes, "mp3")], None);
        wait_until(Duration::from_secs(20), "whole", || feed.is_complete());
        let froms: Vec<u64> = mock.streams().iter().map(|r| r.from).collect();
        eprintln!("{}: requests from {froms:?}; {} bytes sent for {len}", if drop { "drop" } else { "stall" }, mock.sent());
        // One retry at the stall or the drop; a machine under load may add one where the paced
        // body came too slowly for the read timeout, which must go on from where it stopped too.
        assert!(froms.len() >= 2 && froms[0] == 0, "retried: {froms:?}");
        assert!(froms.windows(2).all(|w| w[1] > w[0]), "each retry goes on from further on: {froms:?}");
        assert!(froms.iter().any(|&f| f >= at - 64 * 1024 && f <= at), "went on from where it stopped: {froms:?}");
        let retries = froms.len() as u64 - 1;
        assert!((mock.sent() as u64) < len + retries * 64 * 1024, "nothing fetched twice but a chunk a retry");
        assert_eq!(fs::read(data_path(s.dir(), &info("m", &bytes, "mp3"))).unwrap(), bytes);
        // The feed is whole once every byte is filled; the download then syncs the file and
        // removes its sidecar, a moment later (sooner than a test thread looks, on Windows).
        let sidecar = sidecar_path(s.dir(), &key(&info("m", &bytes, "mp3")));
        wait_until(Duration::from_secs(5), "whole: no sidecar", || !sidecar.exists());
        assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    }
}

/// A server that sends the whole file whatever range is asked: the Opus file (whose open wants
/// its end) still plays, read in order once the server is seen to ignore ranges, never asked
/// from the start again and again.
#[test]
fn a_server_that_ignores_ranges_is_read_in_order() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[2]).unwrap();
    let mock = Mock::start();
    mock.add("o", bytes.clone(), "audio/ogg");
    mock.set(|b| {
        b.ignore_ranges = true;
        b.rate = bytes.len() as u64 / 3;
    });
    let s = streamer(&mock, "noranges");
    let (p, rx) = player();
    let req = s.request(1, &info("o", &bytes, "opus")).unwrap();
    let feed = feed_of(&req);
    p.play(req);
    until(&rx, Duration::from_secs(15), "start", |e| matches!(e, Event::TrackStarted { token: 1 }));
    wait_until(Duration::from_secs(15), "whole", || feed.is_complete());
    let n = mock.streams().len();
    eprintln!("ranges ignored: {n} requests");
    assert!(n <= 3, "asked {n} times");
    assert_eq!(fs::read(data_path(s.dir(), &info("o", &bytes, "opus"))).unwrap(), bytes);
    p.stop();
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn a_server_that_keeps_failing_is_given_up_on_and_said() {
    let mock = Mock::start();
    let bytes = vec![7u8; 100_000];
    mock.add("x", bytes.clone(), "audio/flac");
    mock.set(|b| b.fail_next = 100);
    let s = streamer(&mock, "503");
    let req = s.request(1, &info("x", &bytes, "flac")).unwrap();
    let feed = feed_of(&req);
    s.keep(&[info("x", &bytes, "flac")], None);
    wait_until(Duration::from_secs(10), "failed", || feed.failure().is_some());
    assert_eq!(mock.streams().len(), 4, "the attempts set");
    assert_eq!(feed.failure().as_deref(), Some("the server failed (HTTP 503)"));
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    assert_eq!(s.failure("x").as_deref(), Some("the server failed (HTTP 503)"));
}

#[test]
fn a_missing_file_fails_at_once_and_the_engine_moves_on() {
    let mock = Mock::start();
    let s = streamer(&mock, "gone");
    let (p, rx) = player();
    let bytes = vec![0u8; 5000];
    let req = s.request(1, &info("gone", &bytes, "flac")).unwrap();
    p.play(req);
    let t = Instant::now();
    let e = loop {
        if let e @ Event::Error { .. } = rx.recv_timeout(Duration::from_secs(5)).expect("an event") {
            break e;
        }
    };
    eprintln!("404: {e:?} after {:?}", t.elapsed());
    assert!(matches!(&e, Event::Error { token: 1, message } if message.contains("no longer on the server")), "{e:?}");
    until(&rx, Duration::from_secs(5), "ended", |e| matches!(e, Event::Ended));
    assert_eq!(mock.streams().len(), 1, "not retried");
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    assert_eq!(s.failure("gone").as_deref(), Some("the file is no longer on the server"));
    assert_eq!(s.used(), 0, "nothing left in the cache");
}

#[test]
fn a_cached_file_plays_with_no_request_signed_in_or_not() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[0]).unwrap();
    let mock = Mock::start();
    mock.add("c", bytes.clone(), "audio/flac");
    let s = streamer(&mock, "cached");
    let i = info("c", &bytes, "flac");
    let feed = feed_of(&s.request(1, &i).unwrap());
    s.keep(std::slice::from_ref(&i), None);
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete());
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    let asked = mock.seen().len();
    // A new start of the app on the same folder, signed out.
    let mut cfg = Config::new(s.dir().to_path_buf(), DEFAULT_CAP);
    cfg.read_timeout_ms = 700;
    let again = Streamer::new(cfg);
    let req = again.request(2, &i).unwrap();
    assert!(req.feed.is_none(), "a plain file");
    let (p, rx) = player();
    p.play(req);
    until(&rx, Duration::from_secs(5), "start", |e| matches!(e, Event::TrackStarted { token: 2 }));
    again.set_client(Some(mock.client()));
    assert!(again.request(3, &i).unwrap().feed.is_none());
    assert_eq!(mock.seen().len(), asked, "no request at all");
}

#[test]
fn a_partial_file_after_a_crash_is_completed_never_played_as_whole() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[0]).unwrap();
    let len = bytes.len() as u64;
    let mock = Mock::start();
    mock.add("p", bytes.clone(), "audio/flac");
    let i = info("p", &bytes, "flac");
    let d = dir("crash");
    // What a crash leaves: the first 40% and a stretch near the end on disk, listed in the
    // sidecar; and a claim past what the file holds (never written), which must not count.
    let path = data_path(&d, &i);
    let mut f = fs::File::create(&path).unwrap();
    let head = len * 2 / 5;
    f.write_all(&bytes[..head as usize]).unwrap();
    f.seek(SeekFrom::Start(len - 50_000)).unwrap();
    f.write_all(&bytes[(len - 50_000) as usize..(len - 20_000) as usize]).unwrap();
    drop(f);
    write_sidecar(&sidecar_path(&d, &key(&i)), len, &[0..head, len - 50_000..len], None).unwrap();
    let mut cfg = Config::new(d.clone(), DEFAULT_CAP);
    cfg.read_timeout_ms = 700;
    let s = Streamer::new(cfg);
    // Signed out: a partial file is no file to play.
    assert_eq!(s.request(1, &i).unwrap_err(), Refusal::NoClient);
    s.set_client(Some(mock.client()));
    let req = s.request(2, &i).unwrap();
    let feed = feed_of(&req);
    s.keep(std::slice::from_ref(&i), None);
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete());
    let froms: Vec<u64> = mock.streams().iter().map(|r| r.from).collect();
    eprintln!("crash: requests from {froms:?}, {} bytes fetched of {len}", mock.sent());
    assert!(!froms.contains(&0), "nothing asked from the start again: {froms:?}");
    // What was missing, and at most the end of the chunk that crossed into what was there.
    assert!((mock.sent() as u64) < len - head + 64 * 1024, "only what was missing");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));

    // A file left without its sidecar whose size is not the catalog's: deleted, fetched again.
    fs::write(&path, &bytes[..1000]).unwrap();
    let _ = fs::remove_file(sidecar_path(&d, &key(&i)));
    let req = s.request(3, &i).unwrap();
    assert!(req.feed.is_some(), "not taken for a whole file");
    s.keep(std::slice::from_ref(&i), None);
    wait_until(Duration::from_secs(10), "whole again", || req.feed.as_ref().unwrap().is_complete());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn two_server_tracks_are_gapless() {
    let Some(files) = fixtures(8) else { return };
    let (a, b) = (fs::read(&files[4]).unwrap(), fs::read(&files[5]).unwrap());
    let mock = Mock::start();
    mock.add("a", a.clone(), "audio/flac");
    mock.add("b", b.clone(), "audio/flac");
    // Each in about 1.5 s: the second is there long before the first ends (8 s at ~10x).
    mock.set(|x| x.rate = (a.len() as u64 * 2) / 3);
    let s = streamer(&mock, "gapless");
    let (ia, ib) = (info("a", &a, "flac"), info("b", &b, "flac"));
    let (p, rx) = player();
    p.play(s.request(1, &ia).unwrap());
    p.set_next(Some(s.request(2, &ib).unwrap()));
    s.keep(&[ia, ib], None);
    until(&rx, Duration::from_secs(10), "first", |e| matches!(e, Event::TrackStarted { token: 1 }));
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let e = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("the second track");
        if matches!(e, Event::TrackStarted { token: 2 }) {
            break;
        }
        seen.push(e);
    }
    assert!(!seen.iter().any(|e| matches!(e, Event::Ended | Event::Error { .. } | Event::Buffering { token: 2, on: true })), "gapless: {seen:?}");
    p.stop();
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn eviction_keeps_the_cap_and_the_tracks_kept() {
    let d = dir("evict");
    let mut cfg = Config::new(d.clone(), 10_000);
    let changes = Arc::new(Mutex::new(0));
    let c = changes.clone();
    cfg.changed = Some(Arc::new(move |_| *c.lock().unwrap() += 1));
    let s = Streamer::new(cfg);
    // Five whole files of 3,000 bytes, played in order 0..5 (0 the longest ago).
    let infos: Vec<StreamInfo> = (0..5).map(|n| StreamInfo { song_id: format!("e{n}"), suffix: Some("flac".into()), content_type: None, size: 3000, ..Default::default() }).collect();
    let base = SystemTime::now() - Duration::from_secs(3600);
    for (n, i) in infos.iter().enumerate() {
        let p = data_path(&d, i);
        fs::write(&p, vec![0u8; 3000]).unwrap();
        OpenOptions::new().write(true).open(&p).unwrap().set_modified(base + Duration::from_secs(n as u64 * 60)).unwrap();
    }
    // The oldest is the one playing: kept.
    s.keep(std::slice::from_ref(&infos[0]), None);
    s.evict();
    let left: Vec<bool> = infos.iter().map(|i| data_path(&d, i).exists()).collect();
    assert_eq!(left, vec![true, false, false, true, true], "the least recently played went, not the one kept");
    assert!(s.used() <= 10_000);
    // Played again: the newest now; a lower cap takes the others first.
    assert!(s.request(9, &infos[3]).unwrap().feed.is_none());
    s.keep(&[], None);
    s.set_cap(3000);
    let left: Vec<bool> = infos.iter().map(|i| data_path(&d, i).exists()).collect();
    assert_eq!(left, vec![false, false, false, true, false]);
    assert!(*changes.lock().unwrap() >= 1);
    // Clear keeps the kept; clearing all leaves nothing.
    s.keep(std::slice::from_ref(&infos[3]), None);
    s.clear(false);
    assert!(data_path(&d, &infos[3]).exists());
    s.clear(true);
    assert_eq!(s.used(), 0);
}

#[test]
fn skipping_signing_out_and_quitting_leave_nothing_behind() {
    let Some(files) = fixtures(20) else { return };
    let bytes = fs::read(&files[0]).unwrap();
    let mock = Mock::start();
    mock.add("k", bytes.clone(), "audio/flac");
    mock.add("l", bytes.clone(), "audio/flac");
    // Slow: 20 s for the file.
    mock.set(|b| b.rate = bytes.len() as u64 / 20);
    let s = streamer(&mock, "skip");
    let (ik, il) = (info("k", &bytes, "flac"), info("l", &bytes, "flac"));
    let (p, rx) = player();
    p.play(s.request(1, &ik).unwrap());
    s.keep(std::slice::from_ref(&ik), None);
    until(&rx, Duration::from_secs(10), "start", |e| matches!(e, Event::TrackStarted { token: 1 }));
    assert_eq!(s.running.load(Ordering::SeqCst), 1);
    // Skipped to another: the engine lets go of the first, which is no longer kept.
    let t = Instant::now();
    p.play(s.request(2, &il).unwrap());
    s.keep(std::slice::from_ref(&il), None);
    wait_until(Duration::from_secs(3), "the first download stops", || s.running.load(Ordering::SeqCst) == 1 && !s.lock().downloads.contains_key(&key(&ik)));
    wait_until(Duration::from_secs(3), "its connection closes", || mock.open() == 1);
    eprintln!("skip: the first download and its connection gone after {:?}", t.elapsed());
    // What it wrote stays, to go on from.
    assert!(sidecar_path(s.dir(), &key(&ik)).exists());

    // Sign-out: the playing download fails with words for it, and ends.
    let feed = s.lock().downloads.get(&key(&il)).unwrap().feed.clone();
    s.set_client(None);
    wait_until(Duration::from_secs(3), "signed out", || feed.failure().as_deref() == Some(SIGNED_OUT));
    until(&rx, Duration::from_secs(5), "the track's error", |e| matches!(e, Event::Error { token: 2, .. }) || matches!(e, Event::Ended));
    wait_until(Duration::from_secs(3), "no download", || s.running.load(Ordering::SeqCst) == 0);
    wait_until(Duration::from_secs(3), "no connection", || mock.open() == 0);
    assert_eq!(s.request(3, &ik).unwrap_err(), Refusal::NoClient);

    // Quit while downloading: the thread and the connection go.
    s.set_client(Some(mock.client()));
    let _ = s.request(4, &ik).unwrap();
    wait_until(Duration::from_secs(3), "connected", || mock.open() == 1);
    assert!(s.shutdown_and_wait(Duration::from_secs(3)), "quit: every download ended");
    wait_until(Duration::from_secs(3), "no connection after quit", || mock.open() == 0);
    drop(p);
}

/// Sign-out clears the cache on a thread that first waits for each download to stop (a stalled
/// one: up to its read timeout). Signing in again meanwhile starts a new session: what it fetches
/// in that time stays, and only the old session's files go.
#[test]
fn a_clear_waiting_on_a_stalled_download_leaves_the_next_session_alone() {
    let old: Vec<u8> = (0..200_000u32).map(|n| (n * 7) as u8).collect();
    let new: Vec<u8> = (0..20_000u32).map(|n| (n * 13) as u8).collect();
    let first = Mock::start();
    first.add("a", old.clone(), "audio/flac");
    // Silent from 50 KB on until the streamer gives up on the connection.
    first.set(|b| b.stall_at = Some((50_000, Duration::from_secs(60))));
    let s = streamer(&first, "clear-race");
    let ia = info("a", &old, "flac");
    let _ = s.request(1, &ia).unwrap();
    let pa = data_path(s.dir(), &ia);
    wait_until(Duration::from_secs(5), "the stall reached", || fs::metadata(&pa).is_ok_and(|m| m.len() >= 50_000));

    // Signed out: the clear stops the download and waits for it.
    let (s2, done) = (s.clone(), Arc::new(AtomicBool::new(false)));
    let d2 = done.clone();
    let clearing = std::thread::spawn(move || {
        s2.clear(true);
        d2.store(true, Ordering::SeqCst);
    });
    wait_until(Duration::from_secs(2), "the download told to stop", || s.lock().downloads.get(&key(&ia)).is_none_or(|d| d.stopping()));

    // Signed in to another server before the clear is done, and a track fetched whole.
    let second = Mock::start();
    second.add("b", new.clone(), "audio/flac");
    s.set_client(Some(second.client()));
    let ib = info("b", &new, "flac");
    let feed = feed_of(&s.request(2, &ib).unwrap());
    wait_until(Duration::from_secs(5), "the new track whole", || feed.is_complete() && !sidecar_path(s.dir(), &key(&ib)).exists());
    assert!(!done.load(Ordering::SeqCst), "the clear was still waiting (the test proves nothing otherwise)");
    clearing.join().unwrap();

    assert!(CachedFile::new(s.dir(), &ib, None).whole(), "the new session's track stays");
    assert_eq!(fs::read(data_path(s.dir(), &ib)).unwrap(), new);
    assert!(!pa.exists() && !sidecar_path(s.dir(), &key(&ia)).exists(), "the old session's files went");
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

/// The space used is counted by the thread that changed it (a download, an eviction, a clear)
/// and handed on with the news; what Settings reads is that count, never the folder.
#[test]
fn the_space_used_is_counted_where_it_changes_and_read_without_the_disk() {
    let bytes: Vec<u8> = (0..30_000u32).map(|n| (n * 31) as u8).collect();
    let mock = Mock::start();
    mock.add("u", bytes.clone(), "audio/flac");
    let told = Arc::new(Mutex::new(Vec::<(u64, String)>::new()));
    let t = told.clone();
    let mut cfg = Config::new(dir("counted"), DEFAULT_CAP);
    cfg.changed = Some(Arc::new(move |n| t.lock().unwrap().push((n, std::thread::current().name().unwrap_or("").to_owned()))));
    let s = Streamer::new(cfg);
    s.set_client(Some(mock.client()));
    assert_eq!(s.known_used(), 0);
    let iu = info("u", &bytes, "flac");
    let feed = feed_of(&s.request(1, &iu).unwrap());
    wait_until(Duration::from_secs(5), "whole", || feed.is_complete());
    wait_until(Duration::from_secs(5), "told", || !told.lock().unwrap().is_empty());
    let (n, thread) = told.lock().unwrap()[0].clone();
    assert_eq!(n, bytes.len() as u64, "the whole file, counted");
    assert_eq!(thread, "mp-stream", "counted on the download's thread");
    assert_eq!(s.known_used(), n);

    // A file the streamer did not write: not seen until a worker counts again.
    fs::write(s.dir().join("other.flac"), vec![0u8; 1000]).unwrap();
    assert_eq!(s.known_used(), bytes.len() as u64, "no look at the folder");
    s.set_cap(DEFAULT_CAP);
    assert_eq!(s.known_used(), bytes.len() as u64 + 1000);
    assert_eq!(told.lock().unwrap().last().unwrap().0, bytes.len() as u64 + 1000);
    s.keep(&[], None);
    s.clear(true);
    assert_eq!((s.known_used(), told.lock().unwrap().last().unwrap().0), (0, 0));
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

/// A streamer on `d` (a folder another one may have left files in), signed in to the mock.
fn streamer_on(mock: &Mock, d: &Path) -> Arc<Streamer> {
    let mut cfg = Config::new(d.to_path_buf(), DEFAULT_CAP);
    cfg.read_timeout_ms = 700;
    cfg.backoff = vec![Duration::from_millis(100), Duration::from_millis(200)];
    let s = Streamer::new(cfg);
    s.set_client(Some(mock.client()));
    s
}

/// What a download stopped part of the way leaves in `d`: its partial file and its sidecar
/// (read back). The link is slow, so the stop comes well before the end.
fn stopped_part_way(mock: &Mock, d: &Path, i: &StreamInfo) -> Sidecar {
    mock.set(|b| b.rate = 200_000);
    let s = streamer_on(mock, d);
    let _req = s.request(1, i).unwrap();
    wait_until(Duration::from_secs(10), "some of it", || mock.sent() >= 150_000);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
    mock.set(|b| b.rate = 0);
    serde_json::from_slice(&fs::read(sidecar_path(d, &key(i))).unwrap()).unwrap()
}

/// An unchanged file goes on from its partial file as before (a 206 from where it stopped), the
/// range sent with the validator the first answer gave, which the sidecar keeps.
#[test]
fn a_partial_file_goes_on_with_its_validator() {
    let bytes: Vec<u8> = (0..600_000u32).map(|n| (n * 7 + n / 251) as u8).collect();
    let mock = Mock::start();
    mock.add("v", bytes.clone(), "audio/flac");
    let i = info("v", &bytes, "flac");
    let d = dir("validator");
    let side = stopped_part_way(&mock, &d, &i);
    let validator = mock.streams()[0].clone();
    assert_eq!((validator.from, validator.if_range), (0, None), "the first asks from the start, with nothing to send");
    let lm = side.validator.clone().expect("the sidecar keeps the validator");
    let head = side.filled.first().map_or(0, |r| r.1);
    assert!(head > 0 && head < bytes.len() as u64, "part of it on disk: {:?}", side.filled);

    let before = mock.streams().len();
    let sent = mock.sent();
    let s = streamer_on(&mock, &d);
    let feed = feed_of(&s.request(2, &i).unwrap());
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete());
    let after: Vec<_> = mock.streams()[before..].to_vec();
    assert_eq!(after.len(), 1, "one range for the rest: {after:?}");
    assert_eq!((after[0].from, after[0].if_range.as_deref()), (head, Some(lm.as_str())));
    assert!(((mock.sent() - sent) as u64) <= bytes.len() as u64 - head, "only what was missing");
    assert_eq!(fs::read(data_path(&d, &i)).unwrap(), bytes);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

/// A file replaced on the server with one of the same size after part of it was fetched: the
/// range sent with the old validator is answered with the whole new file (200), which replaces
/// the partial one; nothing of the old is kept, whatever stretch of it was on disk.
#[test]
fn a_partial_file_of_a_file_changed_since_is_written_afresh() {
    let old: Vec<u8> = (0..600_000u32).map(|n| (n * 7 + n / 251) as u8).collect();
    let new: Vec<u8> = old.iter().map(|b| b ^ 0x5a).collect();
    let mock = Mock::start();
    mock.add("c", old.clone(), "audio/flac");
    let i = info("c", &old, "flac");
    let d = dir("changed");
    let side = stopped_part_way(&mock, &d, &i);
    let lm = side.validator.clone().expect("a validator");
    // And a stretch near the end, as a probe for the end of the file would have left.
    let path = data_path(&d, &i);
    let mut f = OpenOptions::new().write(true).open(&path).unwrap();
    let tail = old.len() as u64 - 40_000;
    f.seek(SeekFrom::Start(tail)).unwrap();
    f.write_all(&old[tail as usize..]).unwrap();
    drop(f);
    let mut filled: Vec<Range<u64>> = side.filled.iter().map(|&(a, b)| a..b).collect();
    add_range(&mut filled, tail..old.len() as u64);
    write_sidecar(&sidecar_path(&d, &key(&i)), old.len() as u64, &filled, Some(&lm)).unwrap();

    mock.replace("c", new.clone());
    let before = mock.streams().len();
    let s = streamer_on(&mock, &d);
    let feed = feed_of(&s.request(2, &i).unwrap());
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete() || feed.failure().is_some());
    assert_eq!(feed.failure(), None);
    let after: Vec<_> = mock.streams()[before..].to_vec();
    assert!(fs::read(&path).unwrap() == new, "the new file, nothing of the old");
    assert!(after[0].from > 0 && after[0].if_range.as_deref() == Some(lm.as_str()), "the range asked with the old validator: {after:?}");
    let sidecar = sidecar_path(&d, &key(&i));
    wait_until(Duration::from_secs(5), "no sidecar", || !sidecar.exists());
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

/// A file replaced on the server during its download (the engine may have read the old bytes):
/// the range after a dropped connection comes as the whole new file, and the track fails as for a
/// change of size, its files deleted; asked again, the new file is fetched whole.
#[test]
fn a_file_changed_during_its_download_fails_and_is_fetched_again() {
    let old: Vec<u8> = (0..400_000u32).map(|n| (n * 13 + n / 97) as u8).collect();
    let new: Vec<u8> = old.iter().map(|b| b.wrapping_add(1)).collect();
    let mock = Mock::start();
    mock.add("d", old.clone(), "audio/flac");
    // Replaced while the first answer still sends the old file (by the mock, at that offset:
    // not a moment the test picks, which a slow machine could put after the drop): the range
    // after the drop is asked of the new one.
    mock.set(|b| {
        b.rate = 400_000;
        b.replace_at = Some((50_000, new.clone()));
        b.drop_at = Some(150_000);
    });
    let i = info("d", &old, "flac");
    let d = dir("changed-during");
    let s = streamer_on(&mock, &d);
    let feed = feed_of(&s.request(1, &i).unwrap());
    wait_until(Duration::from_secs(10), "failed", || feed.failure().is_some());
    assert!(feed.failure().unwrap().contains("changed on the server"), "{:?}", feed.failure());
    let froms: Vec<_> = mock.streams().iter().map(|r| (r.from, r.if_range.is_some())).collect();
    assert!(froms.len() >= 2 && froms[1].0 > 0 && froms[1].1, "the retry sent its range with the validator: {froms:?}");
    let path = data_path(&d, &i);
    wait_until(Duration::from_secs(5), "files deleted", || !path.exists() && !sidecar_path(&d, &key(&i)).exists());

    mock.set(|b| b.rate = 0);
    let feed = feed_of(&s.request(2, &i).unwrap());
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete());
    assert_eq!(fs::read(&path).unwrap(), new);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

/// A download whose feed has failed but whose thread has not yet returned (it deletes its files
/// and fails the feed, then returns): a request in between starts a new download, not the failed
/// feed back. As the test above does by chance when its second request comes that soon.
#[test]
fn a_request_after_a_failure_is_not_given_the_failed_feed() {
    let bytes: Vec<u8> = (0..200_000u32).map(|n| (n * 11) as u8).collect();
    let mock = Mock::start();
    mock.add("f", bytes.clone(), "audio/flac");
    let i = info("f", &bytes, "flac");
    let s = streamer(&mock, "failed-not-ended");
    let k = key(&i);
    let failing = Arc::new(Download {
        file: CachedFile::new(s.dir(), &i, None),
        info: i.clone(),
        feed: Feed::new(),
        stop: Mutex::new(None),
        woken: Condvar::new(),
        ended: AtomicBool::new(false),
        thread: Mutex::new(None),
    });
    failing.feed.fail("the file changed on the server while it downloaded: play it again".to_owned());
    s.lock().downloads.insert(k, failing.clone());
    let feed = feed_of(&s.request(2, &i).unwrap());
    assert_eq!(feed.failure(), None, "a fresh feed, not the failed one");
    // Its thread returns now; the new download waited for it, then fetches the file.
    failing.ended.store(true, Ordering::Release);
    wait_until(Duration::from_secs(10), "whole", || feed.is_complete());
    assert_eq!(fs::read(data_path(s.dir(), &i)).unwrap(), bytes);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn a_cap_keeps_mp3_bitrates_separate_and_reuses_a_complete_original() {
    let mock = Mock::start();
    let original = vec![3; 100_000];
    let i = info("capped", &original, "flac");
    let s = streamer(&mock, "capped-cache");
    mock.add("capped", original.clone(), "audio/flac");
    for (kbps, bytes) in [(96, vec![1; 10_000]), (192, vec![2; 23_000])] {
        mock.add_mp3("capped", kbps, bytes.clone());
        s.set_quality(Some(kbps));
        let req = s.request(1, &i).unwrap();
        let feed = feed_of(&req);
        wait_until(Duration::from_secs(5), "encoded stream finished", || feed.is_complete());
        assert_eq!(req.path.file_name().unwrap().to_str().unwrap(), format!("capped-100000-mp3-{kbps}k.mp3"));
        assert_eq!(fs::read(req.path).unwrap(), bytes);
    }

    s.set_quality(Some(96));
    let cached = s.request(1, &i).unwrap();
    assert!(cached.feed.is_none());
    assert_eq!(mock.streams().len(), 2);
    s.set_quality(None);
    let raw = s.request(1, &i).unwrap();
    let feed = feed_of(&raw);
    wait_until(Duration::from_secs(5), "original downloaded", || feed.is_complete() && !s.lock().downloads.contains_key(&key(&i)));
    assert_eq!(raw.path, s.dir().join("capped-100000.flac"));
    assert_eq!(fs::read(&raw.path).unwrap(), original);
    s.set_quality(Some(128));
    let reuse = s.request(1, &i).unwrap();
    assert_eq!(reuse.path, raw.path);
    assert!(reuse.feed.is_none());
    assert_eq!(mock.streams().len(), 3);

    let mut low = info("low", &original, "opus");
    low.bitrate_kbps = Some(96);
    mock.add("low", original, "audio/ogg");
    let direct = s.request(1, &low).unwrap();
    assert_eq!(direct.path.extension().unwrap(), "opus");
    let feed = feed_of(&direct);
    wait_until(Duration::from_secs(5), "low bitrate original downloaded", || feed.is_complete());
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn a_partial_transcode_restarts_at_zero_and_never_splices_encoder_runs() {
    let mock = Mock::start();
    let s = streamer(&mock, "partial-transcode");
    let i = info("live", &[0; 25], "flac");
    let file = CachedFile::new(s.dir(), &i, Some(96));
    let sidecar = sidecar_path(s.dir(), &file.key);
    // A previous run left a sparse file, including bytes beyond the end of the next encoding.
    fs::write(&file.path, vec![7; 190_000]).unwrap();
    write_sidecar(&sidecar, 190_000, &[0..10_000, 180_000..190_000], None).unwrap();

    s.set_quality(Some(96));
    mock.add_mp3("live", 96, vec![2; 160_000]);
    mock.set(|b| b.drop_at = Some(32_000));
    let feed = feed_of(&s.request(1, &i).unwrap());
    wait_until(Duration::from_secs(5), "truncated response failed", || feed.failure().is_some());
    assert!(sidecar.exists() && !file.whole());
    assert_eq!(fs::read(&file.path).unwrap(), vec![2; 32_000]);

    let fresh = vec![4; 20_000];
    mock.add_mp3("live", 96, fresh.clone());
    let feed = feed_of(&s.request(2, &i).unwrap());
    wait_until(Duration::from_secs(5), "fresh encoding finished", || feed.is_complete());
    assert!(file.whole());
    assert_eq!(fs::read(&file.path).unwrap(), fresh);
    let requests = mock.streams();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.from == 0 && r.if_range.is_none()));
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn changing_quality_keeps_the_playing_file_and_prepared_next_and_applies_to_prefetch() {
    let mock = Mock::start();
    let s = streamer(&mock, "quality-and-queue");
    let raw = vec![1; 120_000];
    let now = info("now", &raw, "flac");
    let next = info("next", &raw, "flac");
    let ahead = info("ahead", &raw, "flac");
    for i in [&now, &next] {
        mock.add(&i.song_id, raw.clone(), "audio/flac");
    }
    mock.add_mp3("ahead", 96, vec![6; 13_000]);
    mock.set(|b| b.rate = 45_000);
    let playing = s.request(1, &now).unwrap();
    let prepared = s.request(2, &next).unwrap();
    let pair = [now.clone(), next.clone()];
    s.keep(&pair, Some(ahead.clone()));
    s.set_quality(Some(96));
    s.keep(&pair, Some(ahead.clone()));
    mock.set(|b| b.rate = 0);
    wait_until(Duration::from_secs(5), "originals finish", || feed_of(&playing).is_complete() && feed_of(&prepared).is_complete());
    let prefetched = CachedFile::new(s.dir(), &ahead, Some(96));
    wait_until(Duration::from_secs(5), "next MP3 prefetched", || prefetched.whole());
    assert_eq!(mock.streams().len(), 3);
    s.clear(false);
    assert!(playing.path.exists() && prepared.path.exists() && prefetched.whole());
    assert_eq!(s.request(3, &ahead).unwrap().path, prefetched.path);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}

#[test]
fn a_chunked_mp3_transcode_can_play_before_the_encoding_finishes() {
    let Some(files) = fixtures(20) else { return };
    let mp3 = fs::read(&files[1]).unwrap();
    let mock = Mock::start();
    let s = streamer(&mock, "play-live-mp3");
    mock.add_mp3("lossless", 192, mp3.clone());
    mock.set(|b| b.rate = mp3.len() as u64 / 4);
    s.set_quality(Some(192));
    let req = s.request(1, &info("lossless", &[0; 5], "flac")).unwrap();
    let feed = feed_of(&req);
    let (p, rx) = player();
    p.play(req);
    until(&rx, Duration::from_secs(6), "audible MP3", |e| matches!(e, Event::TrackStarted { token: 1 }));
    assert!(!feed.is_complete(), "first sound before the HTTP stream ends");
    wait_until(Duration::from_secs(10), "MP3 cached", || feed.is_complete());
    assert_eq!(mock.streams().len(), 1);
    assert!(s.shutdown_and_wait(Duration::from_secs(5)));
}
