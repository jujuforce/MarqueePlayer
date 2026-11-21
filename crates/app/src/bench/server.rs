//! `MP_BENCH=server`: a Navidrome server beside the folders, end to end in the real app against
//! a stand-in on 127.0.0.1 (`subsonic::mock`, started here: nothing leaves the machine).
//! `bench-server.txt` and `sv-*.png`:
//!
//! - the stand-in's catalog: two of the library's own albums as the same files (their paths'
//!   last folder and name, their sizes) under other titles, so only the files can match them,
//!   and 30 albums only the server has (most with a cover);
//! - Settings > Library signed out; "Find a setting" finding it by "navidrome"; a plain http
//!   address noted; a wrong password refused in words; the right one: signed in, the password
//!   field emptied, `navidrome.json` without the password (and, on Windows, without the token);
//! - the sync: progress on the sidebar and in Settings while it runs (the stand-in slowed), the
//!   two albums one each (the files' titles), the server's albums in the library, their covers
//!   in the art cache; the log naming neither the address nor the user;
//! - the server's albums and tracks: no Show in Explorer, View metadata "from the server", an
//!   M3U export leaving them out and saying so; Settings > About's counts; Copy diagnostics'
//!   line (without the address or the user);
//! - Sync now listing again; a start's sync asking two questions when the server has not
//!   scanned (its scan status, its stars);
//! - what goes back to the server: the three switches on, found by "Find a setting"; a heart
//!   given here on the server at once (and never a rating), a play reported with its time; a
//!   heart given there here after Sync now (and in the lists); both switches off: no write at all,
//!   whatever changes here, no play queued; on again: what changed meanwhile sent; the Last.fm
//!   switch skipping a server track and not a local one (`sv-12-server-switches`); five tracks
//!   chosen in a playlist (four of the server's, their cloud kept on the chosen rows,
//!   `sv-30-selection-cloud`) favourited from their menu: one `star` per server track, none
//!   for the folder's; the switch off: nothing sent; on again: sent;
//! - Stop mid-sync; the server gone (a quiet line, the library as it was); the shrink
//!   guard (held, then confirmed); the sign-in refused (signed out, the music kept, the section
//!   marked); signing in again; signing out (the server's music gone, the folders' as it was);
//! - what plays from the server marked with a cloud (album pages, tiles, track rows), local and
//!   matched tracks not; the tooltip's host (`Server.host`); `sv-icon-dark.png` and
//!   `sv-icon-light.png` (a playlist of both kinds, in the built-in theme and in light tokens);
//! - the top right's progress (`activity.rs`): the start-up scan, the sync after signing in, Sync
//!   now, a start's sync, one back from the tray, a rescan of the scanned library and a rescan
//!   with a sync right after it: each shown at least 700 ms, ended at full, one after the other
//!   without a gap;
//! - an empty library (the folders removed, signed out): Home offers both sources, "Connect
//!   Navidrome" opens the sign-in focused, and the first sync shows on Home as a first scan does,
//!   its albums coming in while it runs.
//!
//! Needs a music folder (`MP_MUSIC`) of at least two albums of two tracks or more.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, Timer, TimerMode};
use subsonic::mock::{self, Mock, Trouble};

use crate::activity::{self, Job, Shown};
use crate::bench::{snapshot, write_report};
use crate::server::{self, Reason};
use crate::state::State;
use crate::{Activity, Actions, AlbumPage, AppWindow, Home, Meta, Nav, Page, Server, SettingsView, settings, views};

type Report = Rc<RefCell<String>>;

const USER: &str = "bench-listener";
const PASSWORD: &str = "correct horse & battery/staple";

thread_local! {
    static FAILS: Cell<u32> = const { Cell::new(0) };
    static MOCK: RefCell<Option<Mock>> = const { RefCell::new(None) };
}

fn line(r: &Report, text: String) {
    log::info!("server bench: {text}");
    r.borrow_mut().push_str(&text);
    r.borrow_mut().push('\n');
}

fn check(r: &Report, what: &str, ok: bool, detail: String) {
    if !ok {
        FAILS.with(|f| f.set(f.get() + 1));
        crate::bench::count_fail();
    }
    line(r, format!("  [{}] {what}: {detail}", if ok { "ok" } else { "FAIL" }));
}

fn with_mock<R>(f: impl FnOnce(&Mock) -> R) -> R {
    MOCK.with(|m| f(m.borrow().as_ref().expect("the stand-in runs")))
}

fn one(st: &State, sql: &str) -> i64 {
    st.db.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

/// A cover: a small PNG of one colour.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_fn(300, 300, |x, y| image::Rgb([rgb[0].wrapping_add((x / 30) as u8 * 8), rgb[1], rgb[2].wrapping_add((y / 30) as u8 * 8)]));
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// The stand-in's catalog: two local albums as the same files (other titles), and 30 of its own.
fn catalog(st: &State) -> (mock::Library, Vec<(i64, String)>) {
    let mut lib = mock::Library { last_scan: Some("2026-11-01T00:00:00Z".into()), ..Default::default() };
    let mut mirrored = Vec::new();
    let ids: Vec<(i64, String, String)> = st
        .db
        .prepare("SELECT a.id, a.title, a.album_artist FROM albums a WHERE (SELECT COUNT(*) FROM tracks t WHERE t.album_id = a.id) >= 2 ORDER BY a.id LIMIT 2")
        .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect())
        .unwrap_or_default();
    for (n, (id, title, artist)) in ids.into_iter().enumerate() {
        let tracks = library::query::album_tracks(&st.db, id).unwrap_or_default();
        let songs: Vec<(String, u32, u32, u64, String)> = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let p = std::path::Path::new(&t.path);
                let folder = p.parent().and_then(|f| f.file_name()).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                (format!("Server title {i}"), t.track_no.unwrap_or(i as u32 + 1), t.duration.round() as u32, size, format!("Music Share/{}/{folder}/{name}", artist.to_uppercase()))
            })
            .collect();
        let refs: Vec<(&str, u32, u32, u64, &str)> = songs.iter().map(|(a, b, c, d, e)| (a.as_str(), *b, *c, *d, e.as_str())).collect();
        lib.albums.push(mock::album(&format!("mirror{n}"), &format!("{artist} (server spelling)"), &format!("{title} [Server Edition]"), 1999, "2025-01-01T00:00:00Z", &refs));
        mirrored.push((id, title));
    }
    for i in 0..30 {
        let id = format!("only{i:02}");
        let mut a = mock::album(
            &id,
            &format!("Server Artist {}", i % 6),
            &format!("Server Album {i:02}"),
            2000 + i,
            &format!("2026-10-{:02}T12:00:00Z", i + 1),
            &[("First", 1, 61, 1_000_000 + i as u64, ""), ("Second", 2, 75, 2_000_000 + i as u64, "")],
        );
        a.genres = vec![subsonic::Named { name: "Server Genre".into() }];
        if i % 7 != 3 {
            lib.covers.insert(format!("al-{id}"), png([(i * 8) as u8, 120, 200 - (i * 5) as u8]));
        }
        lib.albums.push(a);
    }
    (lib, mirrored)
}

/// What the top right showed (`activity::take_log`): the jobs in turn, how often it went away
/// and came back, how long it stayed from the first show to the last hide, and whether every
/// job ended at full (a done entry, the bar at 1, before it went or the next came).
struct Seen {
    jobs: Vec<Job>,
    gaps: usize,
    shown_ms: Option<u128>,
    ended_full: bool,
    hidden: bool,
}

fn seen(log: &[(Instant, Option<Shown>, f32)]) -> Seen {
    let mut s = Seen { jobs: Vec::new(), gaps: 0, shown_ms: None, ended_full: true, hidden: false };
    let mut first: Option<Instant> = None;
    let mut prev: Option<(Shown, f32)> = None;
    for (i, (at, shown, progress)) in log.iter().enumerate() {
        match shown {
            Some(sh) => {
                first.get_or_insert(*at);
                if s.jobs.last() != Some(&sh.job) {
                    s.jobs.push(sh.job);
                }
                // Another job took over: the one before ended at full (or a scan took over a
                // sync, which goes on after it).
                if let Some((p, pp)) = prev
                    && p.job != sh.job
                    && !(p.job == Job::Sync && sh.job == Job::Scan)
                    && !(p.done && pp >= 0.999)
                {
                    s.ended_full = false;
                }
                prev = Some((*sh, *progress));
            }
            None => {
                if let Some((p, pp)) = prev
                    && !(p.done && pp >= 0.999)
                {
                    s.ended_full = false;
                }
                if prev.is_some() {
                    if log[i + 1..].iter().any(|e| e.1.is_some()) {
                        s.gaps += 1;
                    } else {
                        s.hidden = true;
                        s.shown_ms = first.map(|f| at.duration_since(f).as_millis());
                    }
                }
                prev = None;
            }
        }
    }
    s
}

/// The checks of one job's (or a run's) turn in the top right. `then`: another job may follow
/// (the folder watcher's rescan, which a sync's writes can set off).
fn check_seen(r: &Report, what: &str, jobs: &[Job], then: bool) {
    let s = seen(&activity::take_log());
    line(r, format!("  [top right] {what}: {:?}, {} gaps, shown {:?} ms, ended at full {}", s.jobs, s.gaps, s.shown_ms, s.ended_full));
    check(r, &format!("{what}: shown in the top right"), if then { s.jobs.starts_with(jobs) } else { s.jobs == jobs }, format!("{:?}", s.jobs));
    check(r, &format!("{what}: one after the other, no gap, then gone"), s.gaps == 0 && s.hidden, format!("{} gaps", s.gaps));
    check(r, &format!("{what}: at least 700 ms, ending at full"), s.shown_ms.is_some_and(|m| m >= 690) && s.ended_full, format!("{:?} ms", s.shown_ms));
}

/// Light colour tokens (theme.toml can set any; the built-in theme is dark), for the icon's
/// screenshot in both: what puts the built-in ones back.
fn light(app: &AppWindow) -> impl FnOnce(&AppWindow) + 'static {
    let t = app.global::<crate::Theme>();
    macro_rules! tokens {
        ($($get:ident $set:ident $v:expr),* $(,)?) => {{
            let old = [$(t.$get()),*];
            $(t.$set(slint::Color::from_argb_encoded($v));)*
            move |app: &AppWindow| {
                let t = app.global::<crate::Theme>();
                let mut i = 0;
                $(t.$set(old[i]); i += 1;)*
                let _ = i;
            }
        }};
    }
    tokens!(
        get_bg set_bg 0xfff6f4f9,
        get_surface set_surface 0xffffffff,
        get_surface_hi set_surface_hi 0xffebe7f2,
        get_border set_border 0x1a000000,
        get_text set_text 0xff1b1823,
        get_text_dim set_text_dim 0xff5d5769,
        get_text_faint set_text_faint 0xff767082,
        get_hover set_hover 0x0d000000,
        get_press set_press 0x18000000,
        get_backdrop_tint set_backdrop_tint 0xc0f6f4f9,
        get_bar_tint set_bar_tint 0xd0f6f4f9,
        get_glass_tint set_glass_tint 0xc2ffffff,
    )
}

/// A step: run `f` once `until` holds (or after `timeout` ms, said as a failure), `delay` ms
/// after the previous one.
type Cond = Box<dyn Fn(&Rc<State>, &AppWindow) -> bool>;
type Act = Box<dyn FnOnce(&Rc<State>, &AppWindow, &Report)>;

struct Stage {
    delay: u64,
    until: Option<(&'static str, Cond, u64)>,
    f: Act,
}

fn go_on(stages: Rc<RefCell<VecDeque<Stage>>>, r: Report) {
    let Some(stage) = stages.borrow_mut().pop_front() else { return };
    Timer::single_shot(Duration::from_millis(stage.delay), move || {
        let Stage { until, f, .. } = stage;
        let run = move |r: Report, stages: Rc<RefCell<VecDeque<Stage>>>| {
            crate::state::with(|st| {
                if let Some(app) = st.app.upgrade() {
                    f(st, &app, &r);
                }
            });
            go_on(stages, r);
        };
        match until {
            None => run(r, stages),
            Some((what, cond, timeout)) => {
                let started = Instant::now();
                let timer = Rc::new(Timer::default());
                let t = timer.clone();
                let run = RefCell::new(Some(run));
                timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
                    let ready = crate::state::with(|st| st.app.upgrade().is_some_and(|app| cond(st, &app))).unwrap_or(false);
                    let late = started.elapsed() > Duration::from_millis(timeout);
                    if ready || late {
                        t.stop();
                        if late && !ready {
                            check(&r, &format!("{what} within {timeout} ms"), false, "timed out".into());
                        } else {
                            line(&r, format!("  ({what}: {} ms)", started.elapsed().as_millis()));
                        }
                        if let Some(run) = run.borrow_mut().take() {
                            run(r.clone(), stages.clone());
                        }
                    }
                });
                std::mem::forget(timer);
            }
        }
    });
}

/// No sync, no sign-in, and the top right gone too (it stays a moment after a job's end).
fn idle(st: &Rc<State>, app: &AppWindow) -> bool {
    let s = app.global::<Server>();
    !s.get_busy() && !s.get_signing_in() && !st.scanning.get() && !app.global::<Activity>().get_shown()
}

pub fn run() {
    let r: Report = Rc::default();
    let stages: Rc<RefCell<VecDeque<Stage>>> = Rc::default();
    macro_rules! stage {
        ($delay:expr, $f:expr) => {{
            let f: Act = Box::new($f);
            stages.borrow_mut().push_back(Stage { delay: $delay, until: None, f });
        }};
        ($delay:expr, until $what:expr, $cond:expr, $timeout:expr, $f:expr) => {{
            let f: Act = Box::new($f);
            stages.borrow_mut().push_back(Stage { delay: $delay, until: Some(($what, Box::new($cond), $timeout)), f });
        }};
    }
    let mirrored: Rc<RefCell<Vec<(i64, String)>>> = Rc::default();
    let local_before: Rc<Cell<i64>> = Rc::default();
    let url: Rc<RefCell<String>> = Rc::default();
    // The stand-in's whole catalog with 70 albums more, for the empty library's first sync.
    let full: Rc<RefCell<Option<mock::Library>>> = Rc::default();
    let playlist: Rc<Cell<i64>> = Rc::default();

    // ---- the stand-in, and Settings signed out ---------------------------------------------
    {
        let (mirrored, local_before, url, full) = (mirrored.clone(), local_before.clone(), url.clone(), full.clone());
        stage!(500, move |st, app, r| {
            let start = seen(&activity::take_log());
            check(r, "the start-up scan of the folders showed in the top right", start.jobs.first() == Some(&Job::Scan), format!("{:?}", start.jobs));
            let (lib, m) = catalog(st);
            let mut more = lib.clone();
            for i in 0..70 {
                more.albums.push(mock::album(&format!("more{i:02}"), &format!("Later Artist {}", i % 9), &format!("Later Album {i:02}"), 1990 + i, "2026-11-05T12:00:00Z", &[("One", 1, 90, 3_000_000 + i as u64, "")]));
            }
            more.last_scan = Some("2026-11-10T00:00:00Z".into());
            *full.borrow_mut() = Some(more);
            line(r, format!("[stand-in] {} albums ({} of them the library's own files), {} covers", lib.albums.len(), m.len(), lib.covers.len()));
            check(r, "the library has two albums to mirror", m.len() == 2, format!("{m:?}"));
            *mirrored.borrow_mut() = m;
            local_before.set(one(st, "SELECT COUNT(*) FROM tracks"));
            match Mock::start(USER, PASSWORD, lib) {
                Ok(mock) => {
                    *url.borrow_mut() = mock.url.clone();
                    MOCK.with(|m| *m.borrow_mut() = Some(mock));
                }
                Err(e) => check(r, "the stand-in starts", false, e.to_string()),
            }
            let hits = settings::nav::find("navidrome");
            check(r, "Find a setting finds the server by \"navidrome\"", hits.iter().any(|h| h.label == "Server" && h.section == settings::nav::LIBRARY), format!("{hits:?}"));
            check(r, "and by \"subsonic\"", settings::nav::find("subsonic").iter().any(|h| h.label == "Server"), String::new());
            views::go(st, Page::Settings);
            views::settings_section(st, settings::nav::LIBRARY);
            let s = app.global::<Server>();
            check(r, "signed out at first", !s.get_signed_in() && !s.get_busy() && s.get_leftover().is_empty(), String::new());
        });
    }
    stage!(700, |_st, app, _r| {
        app.global::<Nav>().set_page_scroll(-260.0);
        snapshot(app, "sv-01-signed-out");
    });
    // ---- signing in ------------------------------------------------------------------------
    {
        let url = url.clone();
        stage!(200, move |_st, app, r| {
            let s = app.global::<Server>();
            let u = url.borrow().clone();
            s.set_address(u.clone().into());
            app.global::<Actions>().invoke_server_address_edited(u.into());
            check(r, "a plain http address is noted", s.get_plain_http(), String::new());
            s.set_user(USER.into());
            s.set_password("not the password".into());
            app.global::<Actions>().invoke_server_connect(s.get_address(), s.get_user(), s.get_password());
            check(r, "the password field lets go of it at once", s.get_password().is_empty() && s.get_signing_in(), String::new());
        });
    }
    stage!(100, until "the wrong password answered", |_st, app| !app.global::<Server>().get_signing_in(), 15_000, |_st, app, r| {
        let s = app.global::<Server>();
        check(r, "a wrong password is refused in words", !s.get_signed_in() && s.get_error().contains("did not accept that user name and password"), s.get_error().to_string());
        snapshot(app, "sv-02-wrong-password");
        with_mock(|m| m.set_delay_ms(25));
        s.set_password(PASSWORD.into());
        app.global::<Actions>().invoke_server_connect(s.get_address(), s.get_user(), s.get_password());
    });
    stage!(100, until "signed in, the sync running", |_st, app| app.global::<Server>().get_signed_in() && app.global::<Server>().get_busy(), 15_000, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "signed in", s.get_signed_in() && s.get_error().is_empty(), s.get_title().to_string());
        let file = std::fs::read_to_string(crate::server::account::path(&st.paths.data)).unwrap_or_default();
        check(r, "navidrome.json is kept, without the password", !file.is_empty() && !file.contains(PASSWORD) && !file.to_lowercase().contains("password"), format!("{} bytes", file.len()));
        #[cfg(windows)]
        {
            let token = subsonic::token(PASSWORD, file.split("\"secret\"").nth(1).unwrap_or(""));
            check(r, "its token is sealed (not in the file as is)", !file.contains(&token) && file.contains("dpapi"), String::new());
        }
        line(r, format!("[syncing] {} | {} | sidebar {:?} | {:.2}", s.get_title(), s.get_status(), s.get_short().to_string(), s.get_progress()));
        check(r, "the sidebar says it syncs", s.get_short().starts_with("Syncing"), s.get_short().to_string());
        let a = app.global::<Activity>();
        check(r, "the top right says it syncs with the server", a.get_shown() && a.get_sync() && a.get_title() == "Syncing with the server", format!("{} | {}", a.get_title(), a.get_text()));
        let host = s.get_host().to_string();
        check(r, "the cloud's tooltip names the server without its scheme", host.starts_with("127.0.0.1:") && !host.contains("http") && !host.ends_with('/'), host);
        app.global::<Nav>().set_page_scroll(-260.0);
        snapshot(app, "sv-03-syncing");
    });
    stage!(100, until "the first sync done", idle, 60_000, |st, app, r| {
        with_mock(|m| m.set_delay_ms(0));
        let s = app.global::<Server>();
        line(r, format!("[synced] {} | {} | {}", s.get_title(), s.get_status(), s.get_detail()));
        let f = server::facts(&st.db);
        line(r, format!("  counts {:?}, {} albums on the server", f.counts, f.albums));
        check(r, "the server's own tracks are in the library", f.counts.server == 60, format!("{}", f.counts.server));
        check(r, "the server's copies of the library's files are those tracks, once", f.counts.both >= 4 && f.counts.local + f.counts.both == one(st, "SELECT COUNT(*) FROM tracks WHERE path NOT LIKE 'navidrome:%'"), format!("{:?}", f.counts));
        check(r, "the status names the user, the server and the counts", s.get_title().contains(USER) && s.get_title().contains("Navidrome") && s.get_status().contains("on the server") && s.get_status().contains("Synced"), s.get_status().to_string());
        check(r, "and that plain http is not encrypted", s.get_status().contains("not encrypted"), String::new());
        check(r, "the server's albums are listed once", one(st, "SELECT COUNT(*) FROM albums WHERE remote_id LIKE 'only%'") == 30, String::new());
        snapshot(app, "sv-04-synced");
        check_seen(r, "the sync after signing in", &[Job::Sync], true);
    });
    {
        let (mirrored, local_before) = (mirrored.clone(), local_before.clone());
        stage!(100, move |st, _app, r| {
            for (id, title) in mirrored.borrow().iter() {
                let a = library::query::album(&st.db, *id).ok().flatten();
                check(
                    r,
                    "a mirrored album is the local one, its title the files'",
                    a.as_ref().is_some_and(|a| &a.title == title && a.remote_id.as_deref().is_some_and(|r| r.starts_with("mirror")) && !a.remote_only),
                    format!("{:?}", a.map(|a| (a.title, a.remote_id))),
                );
            }
            check(r, "no file's track lost or doubled", one(st, "SELECT COUNT(*) FROM tracks WHERE path NOT LIKE 'navidrome:%'") == local_before.get(), String::new());
            // The app's own lines: this bench's report lines (which quote the status) left out.
            let log: String = std::fs::read_to_string(st.paths.log()).unwrap_or_default().lines().filter(|l| !l.contains("server bench:")).collect::<Vec<_>>().join("
");
            let port = with_mock(|m| m.url.rsplit(':').next().unwrap_or("").to_owned());
            check(r, "the log names neither the address nor the user", !log.contains(USER) && !log.contains(&format!("127.0.0.1:{port}")) && log.contains("server sync"), String::new());
        });
    }
    stage!(100, until "covers of the server's albums", |st, _app| one(st, "SELECT COUNT(*) FROM albums WHERE remote_id LIKE 'only%' AND art IS NOT NULL") >= 25, 30_000, |st, _app, r| {
        let stems: Vec<String> = st.db.prepare("SELECT art FROM albums WHERE art IS NOT NULL AND folder LIKE 'navidrome:%'").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).unwrap_or_default();
        check(r, "26 covers fetched (4 the server lacks)", stems.len() == 26, format!("{}", stems.len()));
        check(r, "each in the cache's own sizes, backdrop and accent", stems.iter().all(|s| st.art.is_current(s)), String::new());
        let asked = with_mock(|m| m.count("getCoverArt"));
        check(r, "each cover asked once", asked == 30, format!("{asked}"));
    });
    // ---- the cloud on what plays from the server -----------------------------------------------
    stage!(300, |st, _app, _r| {
        views::open_album(st, one(st, "SELECT id FROM albums WHERE remote_id = 'only05'"));
    });
    stage!(1200, |_st, app, r| {
        let p = app.global::<AlbumPage>();
        let rows: Vec<crate::TrackRow> = (0..p.get_tracks().row_count()).filter_map(|i| p.get_tracks().row_data(i)).collect();
        check(r, "a server album's page is marked, and each of its tracks", p.get_album().remote && !rows.is_empty() && rows.iter().all(|t| t.remote), format!("{} rows", rows.len()));
        snapshot(app, "sv-04b-server-album");
    });
    {
        let mirrored = mirrored.clone();
        stage!(300, move |st, _app, _r| {
            if let Some((id, _)) = mirrored.borrow().first() {
                views::open_album(st, *id);
            }
        });
    }
    stage!(1200, |st, app, r| {
        let p = app.global::<AlbumPage>();
        let rows: Vec<crate::TrackRow> = (0..p.get_tracks().row_count()).filter_map(|i| p.get_tracks().row_data(i)).collect();
        check(r, "a local album the server also has is not marked, nor its tracks", !p.get_album().remote && !rows.is_empty() && rows.iter().all(|t| !t.remote), format!("{} rows", rows.len()));
        views::go(st, Page::Albums);
    });
    stage!(1500, |_st, app, r| {
        let g = app.global::<crate::Grid>().get_albums();
        let cards: Vec<crate::AlbumCard> = (0..g.row_count()).filter_map(|i| g.row_data(i)).collect();
        let marked = cards.iter().filter(|c| c.remote).count();
        check(r, "Albums marks the server's albums only", marked > 0 && marked < cards.len(), format!("{marked} of {}", cards.len()));
        snapshot(app, "sv-04c-albums");
    });
    stage!(1500, |st, app, _r| {
        views::go(st, Page::Home);
        app.global::<Nav>().set_page_scroll(0.0);
    });
    stage!(1500, |_st, app, _r| {
        snapshot(app, "sv-05-home");
    });
    // ---- server-only tracks and albums -------------------------------------------------------
    stage!(300, |st, app, r| {
        let album = one(st, "SELECT id FROM albums WHERE remote_id = 'only05'");
        let folder = library::query::album_folder(&st.db, album).ok().flatten().unwrap_or_default();
        let labels = |v: Vec<crate::menus::Entry>| -> Vec<String> {
            v.into_iter().filter_map(|e| if let crate::menus::Entry::Item { label, .. } = e { Some(label) } else { None }).collect()
        };
        let a = labels(crate::menus::build("album", "", 0, album as i32, None, Some(crate::menus::AlbumInfo { artists: vec![], folder }), None));
        let show = crate::platform::system::words().show_in;
        check(r, "a server album's menu has no Show in Explorer", !a.iter().any(|l| l == show) && a.iter().any(|l| l == "View metadata"), format!("{a:?}"));
        let t = library::query::album_tracks(&st.db, album).unwrap_or_default();
        let ti = crate::menus::TrackInfo { path: t.first().map(|t| t.path.clone()).unwrap_or_default(), ..Default::default() };
        let tl = labels(crate::menus::build("track", "album", 0, 0, Some(ti), None, None));
        check(r, "nor its track's", !tl.iter().any(|l| l == show), format!("{tl:?}"));
        if let Some(t) = t.into_iter().next() {
            crate::metadata::view::open_track(st, t);
        }
        let _ = app;
    });
    stage!(1200, |_st, app, r| {
        let m = app.global::<Meta>();
        check(r, "View metadata says it is from the server", m.get_status().contains("On the server only") && m.get_blocks() == "From the server", format!("{} | {}", m.get_status(), m.get_blocks()));
        check(r, "and offers nothing to show in Explorer", m.get_no_file(), String::new());
        snapshot(app, "sv-06-metadata");
    });
    let pl = playlist.clone();
    stage!(300, move |st, _app, r| {
        let playlist = pl;
        crate::metadata::view::close(st);
        let ids: Vec<i64> = st.db.prepare("SELECT id FROM tracks WHERE path NOT LIKE 'navidrome:%' ORDER BY id LIMIT 2").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).unwrap_or_default();
        let server_ids: Vec<i64> = st.db.prepare("SELECT id FROM tracks WHERE path LIKE 'navidrome:%' ORDER BY id LIMIT 3").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).unwrap_or_default();
        let all: Vec<i64> = ids.iter().chain(&server_ids).copied().collect();
        match library::query::create_playlist_with(&st.db, "Mixed sources", &all) {
            Ok(id) => {
                playlist.set(id);
                let path = crate::bench::out_dir().join("server-mixed.m3u8");
                crate::playlists::file::export_file(st, id, &path, false);
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                check(r, "M3U export leaves the server's tracks out", text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).count() == ids.len() && !text.contains("navidrome:"), format!("{} lines", text.lines().count()));
                let pill = crate::state::with(|st| st.app.upgrade().map(|a| a.global::<Nav>().get_flash().to_string())).flatten().unwrap_or_default();
                check(r, "and says how many", pill.contains("left out 3 tracks only on the server"), pill);
            }
            Err(e) => check(r, "a mixed playlist", false, e.to_string()),
        }
        views::open_playlist(st, playlist.get());
    });
    stage!(1500, |_st, app, r| {
        let rows = app.global::<crate::Playlists>().get_tracks();
        let rows: Vec<crate::TrackRow> = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).collect();
        let marked: Vec<bool> = rows.iter().map(|t| t.remote).collect();
        check(r, "a playlist marks its server tracks and not its files", marked == [false, false, true, true, true], format!("{marked:?}"));
        snapshot(app, "sv-icon-dark");
        let restore = light(app);
        Timer::single_shot(Duration::from_millis(400), move || {
            crate::state::with(|st| {
                if let Some(app) = st.app.upgrade() {
                    snapshot(&app, "sv-icon-light");
                    restore(&app);
                }
            });
        });
    });
    stage!(900, |st, _app, _r| {
        views::go(st, Page::Settings);
        views::settings_section(st, settings::nav::ABOUT);
    });
    stage!(1500, |st, app, r| {
        let v = app.global::<crate::AboutView>();
        check(r, "About counts where the tracks are", v.get_sources().contains("only on the server") && v.get_sources().contains("in both"), v.get_sources().to_string());
        let d: String = crate::about::diagnostics(st).lines().filter(|l| !l.contains("server bench:")).collect::<Vec<_>>().join("
");
        let server_line = d.lines().find(|l| l.starts_with("Server:")).unwrap_or("").to_owned();
        let port = with_mock(|m| m.url.rsplit(':').next().unwrap_or("").to_owned());
        check(r, "Copy diagnostics names the server, not its address or user", server_line.contains("signed in to Navidrome") && !d.contains(USER) && !d.contains(&format!("127.0.0.1:{port}")), server_line);
        app.global::<Nav>().set_page_scroll(-700.0);
        snapshot(app, "sv-07-about");
        views::settings_section(st, settings::nav::LIBRARY);
        with_mock(Mock::clear_requests);
        app.global::<Actions>().invoke_server_sync();
    });
    // ---- syncs again ------------------------------------------------------------------------
    stage!(200, until "Sync now done", idle, 30_000, |st, _app, r| {
        let lists = with_mock(|m| m.count("getAlbumList2"));
        check(r, "Sync now lists again", lists == 1, format!("{lists} pages"));
        check_seen(r, "Sync now", &[Job::Sync], false);
        with_mock(Mock::clear_requests);
        server::sync(st, Reason::Start);
    });
    stage!(200, until "a start's sync done", idle, 30_000, |_st, _app, r| {
        let asked = with_mock(Mock::requests);
        check(r, "a start's sync asks two questions when the server has not scanned (its scan, its stars)", asked == ["getScanStatus", "getStarred2"], format!("{asked:?}"));
        check_seen(r, "a start's sync (one quick question)", &[Job::Sync], false);
    });
    // ---- what goes back to the server ---------------------------------------------------------
    stage!(300, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "the three switches are on for a server signed in to", s.get_sync_marks() && s.get_report_plays() && s.get_lastfm_scrobbles(), String::new());
        for (q, label) in [
            ("favourites server", "Sync favourites with the server"),
            ("hearts navidrome", "Sync favourites with the server"),
            ("plays server", "Report plays to the server"),
            ("double scrobble", "Scrobble server tracks from this app"),
        ] {
            let hits = settings::nav::find(q);
            check(r, &format!("Find a setting: \"{q}\""), hits.first().is_some_and(|h| h.label == label && h.section == settings::nav::LIBRARY), format!("{hits:?}"));
        }
        views::settings_section(st, settings::nav::LIBRARY);
        app.global::<Nav>().set_page_scroll(-560.0);
        with_mock(Mock::clear_requests);
        // A heart given here, as the track lists give it.
        let fav = one(st, "SELECT id FROM tracks WHERE remote_id = 'only03-1'");
        let _ = library::query::toggle_favourite_track(&st.db, fav);
        crate::server::write::changed(st, fav);
    });
    stage!(100, |_st, app, _r| {
        snapshot(app, "sv-12-server-switches");
    });
    stage!(100, until "the heart on the server", |_st, _app| with_mock(|m| {
        let mut ok = false;
        m.edit(|l| ok = l.song("only03-1").is_some_and(|s| s.starred.is_some()));
        ok
    }), 10_000, |st, _app, r| {
        let asked = with_mock(Mock::requests);
        check(r, "sent at once: one star, no rating", asked.iter().filter(|q| q.starts_with("star ")).count() == 1 && !asked.iter().any(|q| q.starts_with("setRating")), format!("{asked:?}"));
        // A play counted of a server track: reported, with its time.
        crate::session::record_play(st, one(st, "SELECT id FROM tracks WHERE remote_id = 'only03-1'"));
    });
    stage!(100, until "the play on the server", |_st, _app| with_mock(|m| {
        let mut ok = false;
        m.edit(|l| ok = l.scrobbles.iter().any(|s| s.id == "only03-1" && s.submission));
        ok
    }), 10_000, |st, app, r| {
        let mut sc = Vec::new();
        with_mock(|m| m.edit(|l| sc = l.scrobbles.clone()));
        let now = library::db::now_ms();
        check(r, "the play reported once, at its time", sc.iter().filter(|s| s.id == "only03-1" && s.submission).count() == 1 && sc.iter().all(|s| s.time.is_none_or(|t| (now - t).abs() < 60_000)), format!("{sc:?}"));
        check(r, "and off the queue", one(st, "SELECT COUNT(*) FROM remote_plays") == 0, String::new());
        // Changed on the server: here after Sync now.
        with_mock(|m| {
            m.edit(|l| {
                l.song_mut("only04-1").expect("song").starred = Some(mock::iso(now));
            })
        });
        app.global::<Actions>().invoke_server_sync();
    });
    stage!(300, until "the server's heart here", |st, app| idle(st, app) && {
        let t = one(st, "SELECT id FROM tracks WHERE remote_id = 'only04-1'");
        library::query::is_favourite_track(&st.db, t).unwrap_or(false)
    }, 30_000, |st, app, r| {
        check(r, "what was sent is not sent back", one(st, "SELECT COUNT(*) FROM tracks t JOIN favourite_tracks f ON f.track_id = t.id WHERE t.remote_id = 'only03-1'") == 1, String::new());
        // Both switches off: nothing goes, whatever changes here.
        app.global::<Actions>().invoke_server_toggle(0);
        app.global::<Actions>().invoke_server_toggle(1);
        let s = app.global::<Server>();
        check(r, "both off", !s.get_sync_marks() && !s.get_report_plays() && !st.settings.borrow().server_marks && !st.settings.borrow().server_plays, String::new());
        with_mock(Mock::clear_requests);
        let t = one(st, "SELECT id FROM tracks WHERE remote_id = 'only05-1'");
        let _ = library::query::toggle_favourite_track(&st.db, t);
        crate::server::write::changed(st, t);
        crate::session::record_play(st, t);
        server::sync(st, Reason::Manual);
    });
    stage!(1500, until "a sync with both off", idle, 30_000, |st, app, r| {
        let asked = with_mock(Mock::requests);
        let writes: Vec<&String> = asked.iter().filter(|q| ["star", "unstar", "setRating", "scrobble"].contains(&q.split(' ').next().unwrap_or(""))).collect();
        check(r, "both off: no write reaches the server", writes.is_empty() && !asked.is_empty(), format!("{} requests, writes {writes:?}", asked.len()));
        check(r, "and no star read", !asked.iter().any(|q| q == "getStarred2"), String::new());
        check(r, "no play queued meanwhile", one(st, "SELECT COUNT(*) FROM remote_plays") == 0, String::new());
        // On again: what changed here meanwhile goes.
        app.global::<Actions>().invoke_server_toggle(0);
        app.global::<Actions>().invoke_server_toggle(1);
    });
    stage!(300, until "what changed while off sent", |_st, _app| with_mock(|m| {
        let mut ok = false;
        m.edit(|l| ok = l.song("only05-1").is_some_and(|s| s.starred.is_some()));
        ok
    }), 30_000, |st, app, r| {
        check(r, "favourites on again: in step", app.global::<Server>().get_sync_marks(), String::new());
        // The scrobbling switch (Last.fm and ListenBrainz): a server track is not the app's to
        // scrobble when off; a file only here is.
        app.global::<Actions>().invoke_server_toggle(2);
        let server_track = library::query::track(&st.db, one(st, "SELECT id FROM tracks WHERE remote_id = 'only05-1'")).ok().flatten();
        let local_track = library::query::track(&st.db, one(st, "SELECT id FROM tracks WHERE remote_id IS NULL ORDER BY id LIMIT 1")).ok().flatten();
        let s = st.settings.borrow().clone();
        check(
            r,
            "scrobbling off for server tracks: a server track skipped, a local one scrobbled",
            !s.server_lastfm
                && server_track.as_ref().is_some_and(|t| !crate::connections::scrobble::allowed(&s, t, &st.db))
                && local_track.as_ref().is_some_and(|t| crate::connections::scrobble::allowed(&s, t, &st.db)),
            String::new(),
        );
        app.global::<Actions>().invoke_server_toggle(2);
        check(r, "and on again", st.settings.borrow().server_lastfm && app.global::<Server>().get_lastfm_scrobbles(), String::new());
    });
    // ---- several tracks chosen in a list: one write per server track -----------------------
    stage!(300, until "idle before the selection", idle, 30_000, |st, _app, _r| {
        // A playlist of four server tracks and one of the folders' (which has nothing to send).
        let mut ids: Vec<i64> = ["only10-1", "only10-2", "only11-1", "only11-2"].iter().map(|s| one(st, &format!("SELECT id FROM tracks WHERE remote_id = '{s}'"))).collect();
        ids.insert(2, one(st, "SELECT id FROM tracks WHERE remote_id IS NULL ORDER BY id LIMIT 1"));
        let pl = library::query::create_playlist(&st.db, "Server selection").unwrap_or(0);
        let _ = library::query::add_to_playlist(&st.db, pl, &ids);
        views::refresh_playlists(st);
        views::open_playlist(st, pl);
    });
    stage!(600, |st, app, r| {
        with_mock(Mock::clear_requests);
        let a = app.global::<Actions>();
        a.invoke_select_press("playlist".into(), 0, false, false);
        a.invoke_select_press("playlist".into(), 4, false, true);
        let rows = app.global::<crate::Playlists>().get_tracks();
        let drawn: Vec<(bool, bool)> = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).map(|t| (t.selected, t.remote)).collect();
        check(r, "the five chosen, the server's four still with their cloud", drawn.len() == 5 && drawn.iter().all(|d| d.0) && drawn.iter().filter(|d| d.1).count() == 4, format!("{drawn:?}"));
        crate::menus::request(st, "track", "playlist", 1, 0, 600.0, 300.0);
        let labels = crate::bench::menu_labels(app);
        check(r, "their menu is for five", labels.first().map(String::as_str) == Some("5 tracks"), format!("{labels:?}"));
        let at = labels.iter().position(|l| l == "Add to favourites").unwrap_or(0) as i32;
        crate::menus::activate(st, at, -1);
    });
    stage!(100, |_st, app, _r| {
        snapshot(app, "sv-30-selection-cloud");
    });
    stage!(100, until "the four hearts on the server", |_st, _app| with_mock(|m| {
        let mut ok = false;
        m.edit(|l| {
            ok = ["only10-1", "only10-2", "only11-1", "only11-2"].iter().all(|id| l.song(id).is_some_and(|s| s.starred.is_some()));
        });
        ok
    }), 10_000, |_st, app, r| {
        let asked = with_mock(Mock::requests);
        let stars: Vec<&String> = asked.iter().filter(|q| q.starts_with("star ")).collect();
        let named = |v: &[&String], id: &str| v.iter().filter(|q| q.contains(id)).count();
        let each = ["only10-1", "only10-2", "only11-1", "only11-2"].iter().all(|id| named(&stars, id) == 1);
        check(r, "a favourite of five chosen: one star per server track, no rating", stars.len() == 4 && each && !asked.iter().any(|q| q.starts_with("setRating")), format!("{asked:?}"));
        // The switch off: the same for all five, and nothing goes.
        app.global::<Actions>().invoke_server_toggle(0);
        with_mock(Mock::clear_requests);
    });
    stage!(300, |st, app, r| {
        check(r, "favourites off", !st.settings.borrow().server_marks, String::new());
        crate::menus::request(st, "track", "playlist", 1, 0, 600.0, 300.0);
        let labels = crate::bench::menu_labels(app);
        check(r, "all five favourites now: the menu offers to remove them", labels.iter().any(|l| l == "Remove from favourites"), format!("{labels:?}"));
        let at = labels.iter().position(|l| l == "Remove from favourites").unwrap_or(0) as i32;
        crate::menus::activate(st, at, -1);
    });
    stage!(2000, |st, app, r| {
        let asked = with_mock(Mock::requests);
        let writes: Vec<&String> = asked.iter().filter(|q| ["star", "unstar", "setRating"].contains(&q.split(' ').next().unwrap_or(""))).collect();
        let t = one(st, "SELECT id FROM tracks WHERE remote_id = 'only10-1'");
        check(r, "the switch off: the five changed here, nothing sent", writes.is_empty() && !library::query::is_favourite_track(&st.db, t).unwrap_or(true), format!("writes {writes:?}"));
        app.global::<Actions>().invoke_server_toggle(0);
    });
    stage!(300, until "what the selection changed while off sent", |_st, _app| with_mock(|m| {
        let mut ok = false;
        m.edit(|l| {
            ok = ["only10-1", "only10-2", "only11-1", "only11-2"].iter().all(|id| l.song(id).is_some_and(|s| s.starred.is_none()));
        });
        ok
    }), 30_000, |st, _app, r| {
        check(r, "on again: the five's changes reach the server", st.settings.borrow().server_marks, String::new());
        views::go(st, Page::Home);
    });
    stage!(500, until "idle again", idle, 30_000, |_st, _app, _r| {
        with_mock(|m| m.set_delay_ms(150));
    });
    stage!(100, |_st, app, _r| {
        app.global::<Actions>().invoke_server_sync();
    });
    stage!(900, |_st, app, r| {
        let s = app.global::<Server>();
        check(r, "a sync can be stopped while it runs", s.get_busy(), s.get_status().to_string());
        app.global::<Actions>().invoke_server_stop();
        check(r, "Stop is said", s.get_stopping(), String::new());
    });
    stage!(100, until "the stop", idle, 20_000, |_st, app, r| {
        let pill = app.global::<Nav>().get_flash().to_string();
        check(r, "stopped, what it read kept", pill.starts_with("Sync stopped"), pill);
        with_mock(|m| {
            m.set_delay_ms(0);
            m.set_trouble(Trouble::Down);
        });
        app.global::<Actions>().invoke_server_sync();
    });
    stage!(200, until "the server gone", idle, 30_000, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "a server gone is a quiet line, not a dialog", s.get_signed_in() && !s.get_attention() && s.get_detail().contains("as last synced"), s.get_detail().to_string());
        check(r, "the library as it was", server::facts(&st.db).counts.server == 60, String::new());
        snapshot(app, "sv-08-unreachable");
        // The server lost its disks: one album left, and it scanned.
        with_mock(|m| {
            m.set_trouble(Trouble::None);
            m.edit(|l| {
                l.albums.retain(|a| a.id == "only00");
                l.last_scan = Some("2026-11-03T00:00:00Z".into());
            });
        });
        server::sync(st, Reason::Start);
    });
    stage!(200, until "the shrunk catalog", idle, 30_000, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "most of a catalog gone at once is held back and said", !s.get_held().is_empty() && server::facts(&st.db).counts.server == 60, s.get_held().to_string());
        app.global::<Nav>().set_page_scroll(-260.0);
        snapshot(app, "sv-09-held");
        app.global::<Actions>().invoke_server_confirm_removal();
    });
    stage!(200, until "the removal confirmed", idle, 30_000, |st, app, r| {
        let s = app.global::<Server>();
        let f = server::facts(&st.db);
        check(r, "confirmed: removed, the folders' tracks kept", s.get_held().is_empty() && f.counts.server == 2 && f.counts.both == 0, format!("{:?}", f.counts));
        with_mock(|m| m.set_trouble(Trouble::WrongPassword));
        app.global::<Actions>().invoke_server_sync();
    });
    stage!(200, until "the sign-in refused", idle, 30_000, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "a refused sign-in signs out and asks to sign in again", !s.get_signed_in() && s.get_attention() && s.get_leftover().contains("Sign in again"), s.get_leftover().to_string());
        check(r, "its music stays meanwhile", server::facts(&st.db).counts.server == 2, String::new());
        check(r, "the stored sign-in is gone", !crate::server::account::path(&st.paths.data).exists(), String::new());
        snapshot(app, "sv-10-sign-in-again");
        with_mock(|m| m.set_trouble(Trouble::None));
        s.set_password(PASSWORD.into());
        app.global::<Actions>().invoke_server_connect(s.get_address(), s.get_user(), s.get_password());
    });
    stage!(300, until "signed in again", |_st, app| app.global::<Server>().get_signed_in() && idle(_st, app), 30_000, |st, app, r| {
        let s = app.global::<Server>();
        check(r, "signed in again, nothing marked", s.get_signed_in() && !s.get_attention(), s.get_status().to_string());
        check(r, "the same library", server::facts(&st.db).counts.server == 2, String::new());
        let _ = activity::take_log();
        server::sync(st, Reason::Shown);
    });
    stage!(200, until "the sync back from the tray", idle, 30_000, |st, app, r| {
        check_seen(r, "a sync back from the tray", &[Job::Sync], false);
        crate::scanning::rescan(st);
        let a = app.global::<Activity>();
        check(r, "a rescan of the scanned library shows at once", a.get_shown() && !a.get_sync() && a.get_title() == "Scanning your folders", format!("{} | {}", a.get_title(), a.get_text()));
    });
    stage!(200, until "the rescan", idle, 60_000, |st, _app, r| {
        check_seen(r, "a rescan of the scanned library", &[Job::Scan], false);
        crate::scanning::rescan(st);
        server::sync(st, Reason::Manual);
    });
    stage!(200, until "a rescan and a sync", idle, 60_000, |_st, app, r| {
        check_seen(r, "a rescan with a sync right after it", &[Job::Scan, Job::Sync], false);
        app.global::<Actions>().invoke_server_sign_out();
    });
    {
        let local_before = local_before.clone();
        stage!(200, until "signed out", idle, 30_000, move |st, app, r| {
            let s = app.global::<Server>();
            let f = server::facts(&st.db);
            check(r, "signed out: the server's music gone, the folders' as it was", !s.get_signed_in() && f.counts.server == 0 && f.counts.both == 0 && !f.connected && f.counts.local == local_before.get(), format!("{f:?}"));
            check(r, "the sign-in forgotten", !crate::server::account::path(&st.paths.data).exists() && s.get_leftover().is_empty(), String::new());
            check(r, "and its fields empty: no address, user or password", s.get_address().is_empty() && s.get_user().is_empty() && s.get_password().is_empty(), format!("{:?} {:?}", s.get_address(), s.get_user()));
            check(r, "and what was agreed with it, and no play left for it", one(st, "SELECT COUNT(*) FROM remote_marks") == 0 && one(st, "SELECT COUNT(*) FROM remote_plays") == 0, String::new());
            let pill = app.global::<Nav>().get_flash().to_string();
            check(r, "and said", pill.starts_with("Signed out."), pill);
            app.global::<Nav>().set_page_scroll(-260.0);
            snapshot(app, "sv-11-signed-out");
        });
    }
    // ---- an empty library: both sources offered, the first sync on Home ------------------------
    stage!(300, |st, _app, _r| {
        let n = st.settings.borrow().folders.len();
        for _ in 0..n {
            crate::scanning::remove_folder(st, 0);
        }
        views::go(st, Page::Home);
    });
    stage!(300, until "the folders removed", |st, app| idle(st, app) && app.global::<Home>().get_empty(), 60_000, |st, app, r| {
        check(r, "with no folders and no server, Home is empty", app.global::<Home>().get_empty() && !app.global::<Server>().get_signed_in() && one(st, "SELECT COUNT(*) FROM tracks") == 0, String::new());
        snapshot(app, "sv-12-empty-home");
        app.global::<Actions>().invoke_server_open_sign_in();
    });
    stage!(900, |_st, app, r| {
        let page = app.global::<Nav>().get_page();
        let v = app.global::<SettingsView>();
        check(r, "Connect a server opens Settings > Library", page == Page::Settings && v.get_section() as usize == settings::nav::LIBRARY, format!("{page:?}, section {}", v.get_section()));
        check(r, "and its sign-in took the request (in view, the address focused)", !v.get_focus_server(), String::new());
        snapshot(app, "sv-13-sign-in");
    });
    {
        let (full, url) = (full.clone(), url.clone());
        stage!(200, move |st, app, _r| {
            // (What the top right showed of the sign-out and the folders' removal goes.)
            let _ = activity::take_log();
            let lib = full.borrow_mut().take().unwrap_or_default();
            with_mock(|m| {
                m.edit(|l| *l = lib);
                m.set_delay_ms(60);
            });
            let s = app.global::<Server>();
            s.set_address(url.borrow().clone().into());
            s.set_user(USER.into());
            s.set_password(PASSWORD.into());
            app.global::<Actions>().invoke_server_connect(s.get_address(), s.get_user(), s.get_password());
            views::go(st, Page::Home);
        });
    }
    stage!(50, until "the first sync of an empty library", |_st, app| app.global::<Server>().get_busy() && app.global::<Activity>().get_shown(), 15_000, |_st, app, r| {
        let a = app.global::<Activity>();
        check(r, "an empty Home shows the first sync as it does a first scan", app.global::<Home>().get_empty() && a.get_sync() && a.get_title() == "Syncing with the server", format!("{} | {}", a.get_title(), a.get_text()));
        snapshot(app, "sv-14-empty-home-syncing");
    });
    stage!(50, until "albums on Home while the sync runs", |_st, app| !app.global::<Home>().get_empty(), 30_000, |_st, app, r| {
        check(r, "its albums come in while it runs", app.global::<Server>().get_busy() && app.global::<Home>().get_shelves().row_count() > 0, String::new());
        snapshot(app, "sv-15-home-filling");
    });
    stage!(200, until "the empty library's sync", idle, 60_000, |st, _app, r| {
        with_mock(|m| m.set_delay_ms(0));
        check(r, "the server's music is the library", server::facts(&st.db).counts.server >= 100, format!("{:?}", server::facts(&st.db).counts));
        check_seen(r, "the empty library's first sync", &[Job::Sync], false);
    });
    stage!(500, |_st, _app, r| {
        let fails = FAILS.with(Cell::get);
        line(r, if fails == 0 { "ALL OK".into() } else { format!("{fails} FAILED") });
        write_report("server", &r.borrow());
        MOCK.with(|m| m.borrow_mut().take());
        let _ = slint::quit_event_loop();
    });
    go_on(stages, r);
}
