//! The scripted bench and measurement harness, built only with the `bench` feature. Off
//! unless `MP_BENCH` is set. The modes below are run from this file; the others live in the
//! submodules, one file per subject, each named for what it exercises (the list after this
//! comment). The bench runner (`crates/bench-runner`) runs every mode with its data.
//!
//!
//! - `MP_BENCH=grid`: open All albums and scroll it at constant speed for 10 s, then to its
//!   end and back by the middle button (autoscroll) at 6,000 and 12,000 px/s; report frame
//!   intervals and render times for each, and check that a tile reads its album a few
//!   times, not once for every cover loaded while it is on screen. `MP_BENCH_GRID_PAGE=genres`:
//!   the Genres page instead; `MP_BENCH_GRID_SIZE=2560x1392`: the window that size first.
//! - `MP_BENCH=sidebar`: slide the sidebar in and out 10 times; report frame times
//!   during the slides, and compare snapshots with it open and closed to check that
//!   the content does not reflow.
//! - `MP_BENCH=tour`: visit each page and save a snapshot of it.
//! - `MP_BENCH=skip`: play an album whose second track cannot be played (the first
//!   album of 4 or more tracks), from its first track and then from the bad one, and
//!   check the queue skips the bad track once, never the one after it, and that the
//!   pill names it.
//!
//! - `MP_BENCH=session-save`, then `MP_BENCH=session-check` on the next run (same
//!   `MP_DATA_DIR`): the first plays the first album from its second track with
//!   shuffle and repeat on, gives its first track plays and a playlist entry, seeks,
//!   pauses and quits; the second reports what came back (queue, position, modes,
//!   window, the engine still stopped), whether the history survived (move a folder
//!   between the runs), then plays from the restored position, searches while
//!   "typing", and removes a second library folder (`MP_BENCH_EXTRA`, if set).
//!
//! - `MP_BENCH=guard`: report and snapshot the mass-removal guard's note in Settings
//!   after the startup scan, press its "Remove missing tracks", report the outcome.
//!
//! - `MP_BENCH=idle-paused` / `idle-stopped`: after the startup scan, play the first
//!   album and pause it (or play nothing), hide the window as closing it does, and stay
//!   idle for `MP_BENCH_IDLE_SECS` (default 90) before quitting, so idle CPU time and
//!   wakeups can be measured from outside.
//! - `MP_BENCH=covers`: on All albums, flick the grid 40,000 px down in one second and
//!   stop; report how long until every cover on screen is in memory, and how many
//!   decodes ran meanwhile and after.
//! - `MP_BENCH=playlist`: make a playlist of (up to) 3,000 tracks, open it, and report
//!   how long the page took to fill and to draw its first frame; scroll it deep, go
//!   away and come back, and check the scroll position comes back. Then Shuffle on it
//!   (page and menu) and playlist name clashes: import, drop, rename, create (`bench/playlist.rs`),
//!   then the selection of track lists on it, the album page and the queue (`bench/selection.rs`).
//!
//! - `MP_BENCH=phase4`: the browse pages, the Albums sort and filter, search "See all",
//!   playlist reorder and drop, the new menu entries and settings, each snapshotted; then
//!   the keyboard: key presses go to the window through Slint's own event dispatch
//!   (`Window::dispatch_event`, no OS input, so it works on a window that never has the
//!   foreground), and the report says where the focus went and what the keys did.
//!
//! - `MP_BENCH=phase5`: output devices, Last.fm, formats, playlist files, lyrics, files
//!   from outside, no ratings left (`bench/output_devices.rs`), each snapshotted and reported.
//!
//! - `MP_BENCH=lyrics`: the Lyrics tab's timing offset, Copy and Save (`bench/lyrics.rs`).
//! - `MP_BENCH=output-default` (Linux): the system default as the equalizer, Settings >
//!   Output and PipeWire see it while it changes (`bench/output_default.rs`).
//! - `MP_BENCH=row-hover`: a track row stays hovered and its play button takes the click while
//!   the pointer rests on it (`bench/row_hover.rs`).
//!
//! - `MP_BENCH=zoom` / `zoom-check`: the interface size, and the next start keeping it
//!   (`bench/zoom.rs`).
//!
//! - `MP_BENCH=visualizer` / `viz-idle`: the spectrum visualizer and the online lyrics
//!   (`bench/visualizer.rs`).
//!
//! - `MP_BENCH=details`: technical details and "New playlist…" (`bench/details.rs`; with
//!   `MP_BENCH_DETAILS=off`, the same views with the setting off).
//! - `MP_BENCH=metadata`: "View metadata" (`bench/metadata.rs`), on the fixtures of
//!   `cargo run -p library --example meta_fixtures`.
//! - `MP_BENCH=settings`: the Settings page (`bench/settings.rs`): every section, the states its
//!   list marks, the keyboard, the filter box, history, and other window sizes.
//! - `MP_BENCH=about`: Settings > About (`bench/about.rs`): what it shows and measures, the
//!   Copy line (never put on the clipboard), the keyboard, the tray's About, 960 x 620.
//! - `MP_BENCH=server`: a Navidrome server beside the folders (`bench/server.rs`), against a stand-in
//!   on 127.0.0.1 it starts itself: sign-in, the sync, covers, the server's own tracks, the errors.
//! - `MP_BENCH=listenbrainz`: Settings > Connections > ListenBrainz (`bench/listenbrainz.rs`), against a
//!   stand-in on 127.0.0.1 it starts itself: connecting, now playing and a listen, offline, a
//!   429, the token refused, diagnostics, love, disconnecting.
//! - `MP_BENCH=home` / `library`: the home page at two sizes, and several artists per
//!   track, compilations, disc headers and forgiving tags on the messy fixtures
//!   (`bench/messy_library.rs`).
//! - `MP_BENCH=smart`: smart playlists (`bench/smart.rs`): the editor by keyboard and pointer,
//!   the page, live updating, and where they show.
//!
//! Results and PNG snapshots go to `MP_BENCH_OUT` (default: the current folder). The
//! app quits when the run is over.

// The modes that have a file of their own, named for what they exercise.
mod about; // about: Settings > About
mod autoscroll; // autoscroll, tray: middle-click scrolling and the tray icon
mod bigscan; // bigscan, soak: the first scan of a large library and a long run
mod details; // details: technical details and "New playlist…"
mod editing; // editing: tag editing in View metadata
mod eq; // eq: Settings > Equalizer and the crossfade
mod eq_paste; // eq-paste: the equalizer's Copy and Paste
mod home_shelves; // home-shelves: Home's hero and grid shelves, and the shelf editor's rows
mod listenbrainz; // listenbrainz: Settings > Connections > ListenBrainz
mod lyrics; // lyrics: the Lyrics tab's timing offset, Copy and Save
mod messy_library; // home, library: several artists and genres per track, forgiving tags, the home page
mod metadata; // metadata: View metadata
mod output_default; // output-default (Linux): the system default output device changing
mod output_devices; // phase5: output devices, Last.fm, formats, playlist files, lyrics, no ratings
mod player_bar; // cleanup: the auto-hiding player bar, the queue, sorting, the pill, Settings > Keys
mod playlist; // playlist (second part): Shuffle and playlist name clashes
mod row_hover; // row-hover: a track row's hover under its play button
mod selection; // playlist (last part): the selection of track lists
mod server; // server: a Navidrome server against a stand-in
mod settings; // settings: the Settings page
mod smart; // smart: smart playlists
mod streaming; // streaming: server-only tracks streamed from a mock
mod visualizer; // visualizer, viz-idle: the spectrum visualizer and online lyrics
mod viz_look; // viz-look: the visualizer's halo, pinned preview, opacity and presets
mod viz_styles; // viz-styles, viz-cost: every visualizer style, and what each costs
mod xfade_rates; // xfade-rates: a crossfade between sample rates
mod zoom; // zoom, zoom-check: the interface size

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, RenderingState, Timer, TimerMode};

use crate::state::{self, State};
use crate::{AppWindow, Grid, Nav, Page, Search, playback, views};

#[derive(Default)]
struct Frames {
    before: Option<Instant>,
    /// (presented at, render time)
    frames: Vec<(Instant, Duration)>,
    recording: bool,
}

/// Failed checks of this run, across every bench module (each module's own `check` counts
/// here too). The process exits with code 3 when any failed (`exit_code`), so a runner needs
/// no parsing to tell a pass from a fail (`tools/bench-runner`).
static FAILED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub(crate) fn count_fail() {
    FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn failed() -> u32 {
    FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A check of the modes that only reported text before: counted, and written to the report
/// as `[ok]` or `[FAIL]` with what was seen.
pub(crate) fn verdict(text: &mut String, what: &str, ok: bool, detail: impl std::fmt::Display) {
    if !ok {
        count_fail();
    }
    text.push_str(&format!("  [{}] {what}: {detail}\n", if ok { "ok" } else { "FAIL" }));
}

/// The process's exit code at the end of a bench run: 0 when every check passed, 3 when one
/// failed (a panic exits with 101, a run the runner had to stop is its own timeout).
pub fn exit_code() -> Option<i32> {
    std::env::var_os("MP_BENCH")?;
    let n = failed();
    log::info!("bench run over: {n} failed checks");
    Some(if n == 0 { 0 } else { 3 })
}

pub(crate) fn out_dir() -> PathBuf {
    let d = std::env::var_os("MP_BENCH_OUT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&d);
    d
}

pub(crate) fn window_pixels(app: &AppWindow) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
    #[cfg(target_os = "macos")]
    {
        let mut frame = crate::visualizer::macos::snapshot(app).or_else(|| app.window().take_snapshot().ok())?;
        unsafe extern "C" { fn mp_transport_snapshot(rgba: *mut u8, width: u32, height: u32); }
        // SAFETY: the main-thread presenter paints over this whole, uniquely borrowed RGBA buffer.
        unsafe { mp_transport_snapshot(frame.make_mut_bytes().as_mut_ptr(), frame.width(), frame.height()) };
        Some(frame)
    }
    #[cfg(not(target_os = "macos"))]
    app.window().take_snapshot().ok()
}

pub(crate) fn snapshot(app: &AppWindow, name: &str) -> Option<image::RgbaImage> {
    let buf = window_pixels(app)?;
    let img = image::RgbaImage::from_raw(buf.width(), buf.height(), buf.as_bytes().to_vec())?;
    let path = out_dir().join(format!("{name}.png"));
    if let Err(e) = img.save(&path) {
        log::warn!("snapshot {name}: {e}");
    }
    Some(img)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn report(name: &str, frames: &[(Instant, Duration)]) -> String {
    let mut intervals: Vec<f64> = frames.windows(2).map(|w| (w[1].0 - w[0].0).as_secs_f64() * 1000.0).collect();
    let mut render: Vec<f64> = frames.iter().map(|f| f.1.as_secs_f64() * 1000.0).collect();
    intervals.sort_by(f64::total_cmp);
    render.sort_by(f64::total_cmp);
    let median = percentile(&intervals, 0.5);
    let long = intervals.iter().filter(|&&i| i > median * 1.5).count();
    let span = frames.last().zip(frames.first()).map_or(0.0, |(l, f)| (l.0 - f.0).as_secs_f64());
    // Frames that would miss a 144 Hz or a 240 Hz display's next refresh on their own (the
    // bench's window is on a 60 Hz one, where neither shows).
    let over = |ms: f64| render.iter().filter(|&&r| r > ms).count();
    format!(
        "{name}: {} frames in {span:.2} s ({:.1} fps)\n  frame interval ms: p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}\n  intervals > 1.5x median: {long} ({:.2}%)\n  render (CPU, before->after) ms: p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}\n  render over 4 ms: {} frames, over 6.9 ms: {}; {:.0} ms in all\n",
        frames.len(),
        frames.len() as f64 / span.max(1e-9),
        median,
        percentile(&intervals, 0.95),
        percentile(&intervals, 0.99),
        intervals.last().copied().unwrap_or(0.0),
        100.0 * long as f64 / intervals.len().max(1) as f64,
        percentile(&render, 0.5),
        percentile(&render, 0.95),
        percentile(&render, 0.99),
        render.last().copied().unwrap_or(0.0),
        over(4.0),
        over(6.9),
        render.iter().sum::<f64>(),
    )
}

pub(crate) fn write_report(name: &str, text: &str) {
    log::info!("\n{text}");
    eprintln!("{text}");
    let _ = std::fs::write(out_dir().join(format!("bench-{name}.txt")), text);
}

/// A sequence of steps, each run after a delay.
pub(crate) type Step = (u64, Box<dyn FnOnce(&Rc<State>, &AppWindow)>);

pub(crate) fn run_steps(steps: Vec<Step>) {
    let steps = Rc::new(RefCell::new(steps.into_iter()));
    fn next(steps: Rc<RefCell<std::vec::IntoIter<Step>>>) {
        let Some((delay, f)) = steps.borrow_mut().next() else { return };
        Timer::single_shot(Duration::from_millis(delay), move || {
            state::with(|st| {
                if let Some(app) = st.app.upgrade() {
                    f(st, &app);
                }
            });
            next(steps);
        });
    }
    next(steps);
}

pub fn install(app: &AppWindow) {
    // A bench build writes no music file but the editor's fixtures (a folder marked by
    // `examples/edit_fixtures`): whatever MP_MUSIC says, every other path is refused.
    library::edit::atomic::set_guard(|p| library::edit::atomic::under_marker(p, ".mp-edit-fixtures"));
    let Ok(mode) = std::env::var("MP_BENCH") else { return };
    if mode == "zoom-check" {
        // From the start: when the saved interface size is first applied.
        crate::bench::zoom::watch_start();
    }
    // A deliberate panic, on the UI thread or on another, two seconds in: what the user is told,
    // what the log keeps (`crash`; the runner's `crash` scenario checks both).
    if mode == "crash-ui" || mode == "crash-worker" {
        let ui = mode == "crash-ui";
        Timer::single_shot(Duration::from_secs(2), move || {
            if ui {
                panic!("bench: a deliberate panic on the UI thread");
            }
            let _ = std::thread::Builder::new().name("mp-bench-crash".into()).spawn(|| panic!("bench: a deliberate panic on a worker thread"));
        });
        return;
    }
    // The first scan of a large library is watched while it runs, not after.
    if mode == "bigscan" {
        crate::bench::bigscan::run_bigscan();
        return;
    }
    let frames = Rc::new(RefCell::new(Frames::default()));
    // A rendering notifier forces Skia to invalidate the entire surface. Install
    // it only for scenarios which actually measure individual Slint draws.
    if matches!(mode.as_str(), "grid" | "sidebar" | "playlist") {
        let frames = frames.clone();
        let res = app.window().set_rendering_notifier(move |state, _| {
            let mut f = frames.borrow_mut();
            match state {
                RenderingState::BeforeRendering => f.before = Some(Instant::now()),
                RenderingState::AfterRendering => {
                    if let Some(b) = f.before.take()
                        && f.recording
                    {
                        f.frames.push((Instant::now(), b.elapsed()));
                    }
                }
                _ => {}
            }
        });
        if let Err(e) = res {
            log::warn!("rendering notifier unavailable: {e:?}");
        }
    }
    // A timer that keeps requesting frames while recording, so idle frames are not
    // mistaken for dropped ones (Slint only redraws when something changes).
    let keepalive = Rc::new(Timer::default());

    // Start once the startup scan is done, so the library is complete.
    let waiter = Rc::new(Timer::default());
    let w = waiter.clone();
    waiter.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        if state::with(|st| st.scanning.get()).unwrap_or(true) {
            return;
        }
        w.stop();
        match mode.as_str() {
            "grid" => grid(frames.clone(), keepalive.clone()),
            "sidebar" => sidebar(frames.clone(), keepalive.clone()),
            "gapless" => gapless(),
            "skip" => skip(),
            "theme" => theme_reload(),
            "session-save" => session_save(),
            "session-check" => session_check(),
            "guard" => guard(),
            "idle-paused" => idle(true),
            "idle-stopped" => idle(false),
            "covers" => covers_after_flick(),
            "playlist" => big_playlist(frames.clone()),
            "phase4" => phase4(),
            "phase5" if std::env::var("MP_BENCH_PART").as_deref() == Ok("open") => crate::bench::output_devices::run_open(),
            "phase5" => crate::bench::output_devices::run(),
            "cleanup" => crate::bench::player_bar::run(),
            "visualizer" => crate::bench::visualizer::run(),
            "settings" => crate::bench::settings::run(),
            "viz-idle" => crate::bench::visualizer::run_idle(),
            "playback-cost" => crate::bench::visualizer::playback_cost(),
            "autoscroll" => crate::bench::autoscroll::run(),
            "tray" => crate::bench::autoscroll::run_tray(),
            "details" => crate::bench::details::run(),
            "metadata" => crate::bench::metadata::run(),
            "about" => crate::bench::about::run(),
            "home" => crate::bench::messy_library::run(),
            "home-shelves" => crate::bench::home_shelves::run(),
            "library" => crate::bench::messy_library::run_library(),
            "editing" => crate::bench::editing::run(),
            "smart" => crate::bench::smart::run(),
            "eq" => crate::bench::eq::run(),
            "viz-styles" => crate::bench::viz_styles::run(),
            "viz-cost" => crate::bench::viz_styles::run_cost(),
            "soak" => crate::bench::bigscan::run_soak(),
            "xfade-rates" => crate::bench::xfade_rates::run(),
            "eq-paste" => crate::bench::eq_paste::run(),
            "viz-look" => crate::bench::viz_look::run(),
            "server" => crate::bench::server::run(),
            "streaming" => crate::bench::streaming::run(),
            "lyrics" => crate::bench::lyrics::run(),
            "zoom" => crate::bench::zoom::run(),
            "zoom-check" => crate::bench::zoom::run_check(),
            "output-default" => crate::bench::output_default::run(),
            "row-hover" => crate::bench::row_hover::run(),
            "listenbrainz" => crate::bench::listenbrainz::run(),
            _ => tour(),
        }
    });
    // The timer holds itself through `w`; keep one handle for the app's lifetime.
    std::mem::forget(waiter);
}

fn start_recording(frames: &Rc<RefCell<Frames>>) {
    let mut f = frames.borrow_mut();
    f.frames.clear();
    f.recording = true;
}

fn stop_recording(frames: &Rc<RefCell<Frames>>) -> Vec<(Instant, Duration)> {
    let mut f = frames.borrow_mut();
    f.recording = false;
    std::mem::take(&mut f.frames)
}

/// See `MP_BENCH=grid` above.
fn grid(frames: Rc<RefCell<Frames>>, scroller: Rc<Timer>) {
    // `MP_BENCH_GRID_PAGE=genres`: the Genres page instead (the same grid, other tiles).
    let genres = std::env::var("MP_BENCH_GRID_PAGE").as_deref() == Ok("genres");
    let scroll = move |app: &AppWindow| if genres { app.global::<Nav>().get_page_scroll() } else { app.global::<Grid>().get_content_y() };
    let scroll_to = move |app: &AppWindow, y: f32| if genres { app.global::<Nav>().set_page_scroll(y) } else { app.global::<Grid>().set_content_y(y) };
    let f1 = frames.clone();
    let f2 = frames.clone();
    let s1 = scroller.clone();
    let text: Rc<RefCell<String>> = Rc::default();
    let (t2, t3) = (text.clone(), text.clone());
    let reads: Rc<Cell<usize>> = Rc::default();
    let r1 = reads.clone();
    // `MP_BENCH_GRID_SIZE=2560x1392`: the window that size (physical pixels) first, for the
    // columns and rows of a larger monitor than the one the bench's window opens on.
    let size: Option<(i32, i32)> = std::env::var("MP_BENCH_GRID_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    });
    let mut steps: Vec<Step> = vec![
        (1500, Box::new(move |st, _| {
            if let Some((w, h)) = size {
                crate::platform::devwindow::resize_client(st.hwnd.get(), w, h);
            }
            views::go(st, if genres { Page::Genres } else { Page::Albums });
        })),
        (2000, Box::new(move |_, app| {
            snapshot(app, "grid-top");
            start_recording(&f1);
            r1.set(crate::covers::row_reads());
            let weak = app.as_weak();
            let start = Instant::now();
            // 2400 px/s: brisk wheel-flick speed, so new rows (and cover loads) keep
            // arriving every frame.
            s1.start(TimerMode::Repeated, Duration::from_millis(1), move || {
                if let Some(app) = weak.upgrade() {
                    scroll_to(&app, -(start.elapsed().as_secs_f32() * 2400.0));
                }
            });
        })),
        (10_000, Box::new(move |_, app| {
            scroller.stop();
            let data = stop_recording(&f2);
            snapshot(app, "grid-scrolled");
            let n = if genres { app.global::<crate::Browse>().get_groups_count() } else { app.global::<Grid>().get_count() };
            let size = app.window().size();
            let mut text = format!("{} in grid: {n}, window {} x {}\n{}", if genres { "genres" } else { "albums" }, size.width, size.height, report("grid scroll 2400 px/s", &data));
            if !genres {
                verdict(&mut text, "the grid lists the library's albums", n > 300, n);
            }
            verdict(&mut text, "the grid scrolled", scroll(app) < -1000.0, scroll(app));
            // A tile reads its album as it comes on screen and again when its cover has
            // loaded, not once more for every other cover that loads meanwhile: that had
            // every tile on screen laid out again on each frame a cover came in, and the
            // grid stutter on a large window.
            let theme = app.global::<crate::Theme>();
            let row_h = theme.get_cover_size() + 50.0 + 24.0;
            let viewport = size.to_logical(app.window().scale_factor()).height - (theme.get_page_top() + 56.0);
            let passed = grid_max_scroll(app, n.max(0) as usize).min(24_000.0) + viewport;
            let tiles = (((passed / row_h).ceil() as usize) * grid_cols(app)).clamp(1, n.max(1) as usize);
            let per_tile = (crate::covers::row_reads() - reads.get()) as f64 / tiles as f64;
            verdict(&mut text, "a tile reads its album a few times, not at every cover loaded", per_tile <= 3.0, format!("{per_tile:.1} reads a tile ({tiles} tiles)"));
            *t2.borrow_mut() = text;
            scroll_to(app, 0.0);
        })),
    ];
    // Then by the middle button (`autoscroll.rs`), down to the end and back up, at two
    // speeds: the pointer 374 px from the anchor (6,000 px/s on the default curve) and
    // 560 px (the fastest, 12,000). Rows come and go several times faster than above.
    for (offset, half) in [(374.0, 4600), (560.0, 2600)] {
        let (f1, f2, text) = (frames.clone(), frames.clone(), text.clone());
        steps.push((1000, Box::new(move |_, app| {
            let w = app.window().size().to_logical(app.window().scale_factor()).width;
            autoscroll::move_to(app, w / 2.0, 300.0);
            autoscroll::click(app, crate::autoscroll::Button::Middle);
            autoscroll::move_to(app, w / 2.0, 300.0 + offset);
            start_recording(&f1);
        })));
        steps.push((half, Box::new(move |_, app| {
            let w = app.window().size().to_logical(app.window().scale_factor()).width;
            let v = app.global::<crate::Autoscroll>().get_vy();
            let y = scroll(app);
            // (A grid of a few tiles, the fixtures' genres, has nowhere to go.)
            let n = if genres { app.global::<crate::Browse>().get_groups_count() } else { app.global::<Grid>().get_count() };
            let far = grid_max_scroll(app, n.max(0) as usize) > 1000.0;
            verdict(&mut text.borrow_mut(), "the middle button scrolled the grid", y < -1000.0 || !far, format!("at {y:.0}, {v:.0} px/s"));
            autoscroll::move_to(app, w / 2.0, 300.0 - offset);
        })));
        let text = t3.clone();
        steps.push((half, Box::new(move |_, app| {
            let data = stop_recording(&f2);
            let v = app.global::<crate::Autoscroll>().get_vy().abs();
            text.borrow_mut().push_str(&report(&format!("grid autoscroll {v:.0} px/s, down and back"), &data));
            autoscroll::click(app, crate::autoscroll::Button::Middle);
            scroll_to(app, 0.0);
        })));
    }
    steps.push((500, Box::new(move |_, _| {
        write_report("grid", &t3.borrow());
        let _ = slint::quit_event_loop();
    })));
    run_steps(steps);
}

fn sidebar(frames: Rc<RefCell<Frames>>, keepalive: Rc<Timer>) {
    let mut steps: Vec<Step> = vec![(1500, Box::new(|st, _| views::go(st, Page::Home)))];
    let shots: Rc<RefCell<Vec<image::RgbaImage>>> = Rc::default();
    {
        let shots = shots.clone();
        steps.push((1500, Box::new(move |_, app| {
            if let Some(i) = snapshot(app, "sidebar-closed") {
                shots.borrow_mut().push(i);
            }
        })));
    }
    {
        let f = frames.clone();
        let k = keepalive.clone();
        steps.push((100, Box::new(move |_, app| {
            start_recording(&f);
            let weak = app.as_weak();
            k.start(TimerMode::Repeated, Duration::from_millis(2), move || {
                if let Some(app) = weak.upgrade() {
                    app.window().request_redraw();
                }
            });
        })));
    }
    for _ in 0..10 {
        steps.push((600, Box::new(|_, app| app.global::<Nav>().set_sidebar_forced(true))));
        steps.push((600, Box::new(|_, app| app.global::<Nav>().set_sidebar_forced(false))));
    }
    {
        let f = frames.clone();
        let shots = shots.clone();
        steps.push((600, Box::new(move |_, app| {
            keepalive.stop();
            let data = stop_recording(&f);
            app.global::<Nav>().set_sidebar_forced(true);
            let mut text = report("sidebar slides (10 in, 10 out, continuous redraw)", &data);
            // Reflow check after the slide settles.
            let shots = shots.clone();
            let weak = app.as_weak();
            Timer::single_shot(Duration::from_millis(800), move || {
                let Some(app) = weak.upgrade() else { return };
                if let (Some(open), Some(closed)) = (snapshot(&app, "sidebar-open"), shots.borrow().first().cloned()) {
                    let scale = app.window().scale_factor();
                    // Compare everything right of the sidebar and its shadow.
                    let x0 = ((app.global::<crate::Theme>().get_sidebar_width() + 48.0) * scale) as u32;
                    let (w, h) = (open.width().min(closed.width()), open.height().min(closed.height()));
                    let mut same = 0u64;
                    let mut total = 0u64;
                    for y in 0..h {
                        for x in x0..w {
                            total += 1;
                            if open.get_pixel(x, y) == closed.get_pixel(x, y) {
                                same += 1;
                            }
                        }
                    }
                    text.push_str(&format!(
                        "  reflow check: {:.3}% of pixels right of x={x0}px identical with the sidebar open vs closed\n",
                        100.0 * same as f64 / total.max(1) as f64
                    ));
                    verdict(&mut text, "the page does not reflow under the sliding sidebar", same as f64 >= 0.999 * total.max(1) as f64, format!("{same} of {total}"));
                }
                write_report("sidebar", &text);
                let _ = slint::quit_event_loop();
            });
        })));
    }
    run_steps(steps);
}

/// What the skip run saw: the queue index and pill text, each time either changed.
#[derive(Default)]
struct SkipTrace {
    seen: Vec<(u128, Option<usize>, String)>,
    snapped: bool,
}

/// Play the album from `start` and watch the queue for `ms`, then call `done` with
/// the queue indices it went through and the notes the pill showed.
fn watch_skip(start: usize, ms: u64, seek_end: bool, done: impl FnOnce(Vec<Option<usize>>, Vec<String>) + 'static) {
    let trace: Rc<RefCell<SkipTrace>> = Rc::default();
    let begun = Instant::now();
    state::with(|st| {
        // MP_BENCH_SKIP_ALBUM names the album (by title); else the first of four tracks or more.
        let id: Option<i64> = match std::env::var("MP_BENCH_SKIP_ALBUM") {
            Ok(title) => st.db.query_row("SELECT id FROM albums WHERE title = ?1", [title], |r| r.get(0)).ok(),
            Err(_) => st
                .db
                .query_row("SELECT album_id FROM tracks GROUP BY album_id HAVING COUNT(*) >= 4 ORDER BY album_id LIMIT 1", [], |r| r.get(0))
                .ok(),
        };
        let tracks = id.map(|id| library::query::album_tracks(&st.db, id).unwrap_or_default()).unwrap_or_default();
        playback::play_tracks(st, tracks, start);
    });
    let poll = Rc::new(Timer::default());
    let t = trace.clone();
    poll.start(TimerMode::Repeated, Duration::from_millis(20), move || {
        state::with(|st| {
            let Some(app) = st.app.upgrade() else { return };
            let idx = st.queue.borrow().current_index();
            let flash = app.global::<Nav>().get_flash().to_string();
            let mut t = t.borrow_mut();
            if t.seen.last().is_none_or(|(_, i, f)| *i != idx || *f != flash) {
                t.seen.push((begun.elapsed().as_millis(), idx, flash.clone()));
            }
            if !flash.is_empty() && !t.snapped {
                t.snapped = true;
                drop(t);
                snapshot(&app, &format!("skip-from-{start}"));
            }
        });
    });
    if seek_end {
        // Run the first track out quickly: the bad one is then reached as the gapless
        // next track (pre-opened while the first played).
        Timer::single_shot(Duration::from_millis(1200), || {
            state::with(|st| {
                let d = st.player.status().duration.unwrap_or(0.0);
                st.player.seek((d - 1.0).max(0.0));
            });
        });
    }
    Timer::single_shot(Duration::from_millis(ms), move || {
        drop(poll);
        let t = trace.borrow();
        let mut idx: Vec<Option<usize>> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut text = format!("from track {start}:\n");
        for (at, i, f) in &t.seen {
            text.push_str(&format!("  {at:>6} ms  queue index {i:?}  pill {f:?}\n"));
            if idx.last() != Some(i) {
                idx.push(*i);
            }
            if !f.is_empty() && !notes.contains(f) {
                notes.push(f.clone());
            }
        }
        log::info!("{text}");
        SKIP_LOG.with(|l| l.borrow_mut().push_str(&text));
        done(idx, notes);
    });
}

thread_local! {
    static GUARD_BEFORE: Cell<i64> = const { Cell::new(0) };
    static EXTRA_COUNTS: Cell<i64> = const { Cell::new(0) };
}

thread_local! {
    static SKIP_LOG: RefCell<String> = const { RefCell::new(String::new()) };
}

/// See `MP_BENCH=skip` above. Track 2 (index 1) of the album must be unplayable.
fn skip() {
    watch_skip(0, 6000, true, |idx, notes| {
        let first = format!(
            "{}: from the first track the queue went {idx:?}, pill {notes:?}\n",
            if idx.ends_with(&[Some(2)]) && !idx.contains(&Some(3)) && notes.iter().any(|n| n.starts_with("Can") || n.contains("stopped early")) {
                "PASS"
            } else {
                count_fail();
                "FAIL"
            }
        );
        Timer::single_shot(Duration::from_millis(5000), move || {
            watch_skip(1, 4000, false, move |idx, notes| {
                let mut text = SKIP_LOG.with(|l| l.borrow().clone());
                text.push_str(&first);
                text.push_str(&format!(
                    "{}: from the bad track the queue went {idx:?}, pill {notes:?}\n",
                    if idx.ends_with(&[Some(2)]) && !idx.contains(&Some(3)) && notes.iter().any(|n| n.starts_with("Can") || n.contains("stopped early")) {
                        "PASS"
                    } else {
                        count_fail();
                        "FAIL"
                    }
                ));
                write_report("skip", &text);
                let _ = slint::quit_event_loop();
            });
        });
    });
}

/// Play a multi-track album through the app (queue, sync_next, engine), seek to 3 s
/// before the end of track 1, and check the app moved on to track 2 by itself.
fn gapless() {
    let log: Rc<RefCell<String>> = Rc::default();
    let l1 = log.clone();
    let l2 = log.clone();
    let l3 = log.clone();
    let steps: Vec<Step> = vec![
        (500, Box::new(move |st, _| {
            let id: Option<i64> = st
                .db
                .query_row(
                    "SELECT album_id FROM tracks WHERE path LIKE '%.opus' GROUP BY album_id HAVING COUNT(*) >= 3 ORDER BY album_id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            let tracks = id.map(|id| library::query::album_tracks(&st.db, id).unwrap_or_default()).unwrap_or_default();
            l1.borrow_mut().push_str(&format!(
                "album {:?}: {}
",
                id,
                tracks.iter().take(3).map(|t| format!("{} ({:.2} s)", t.title, t.duration)).collect::<Vec<_>>().join(", ")
            ));
            playback::play_tracks(st, tracks, 0);
        })),
        (1500, Box::new(move |st, _| {
            let s = st.player.status();
            let d = s.duration.unwrap_or(0.0);
            l2.borrow_mut().push_str(&format!("track 1 playing: token {:?} at {:.2} / {:.2} s; seeking to {:.2}
", s.token, s.position, d, d - 3.0));
            st.player.seek(d - 3.0);
        })),
        (6000, Box::new(move |st, _| {
            let s = st.player.status();
            let q = st.queue.borrow();
            let cur = q.current_index();
            let title = q.current().map(|i| i.track.title.clone()).unwrap_or_default();
            drop(q);
            let mut text = l3.borrow().clone();
            text.push_str(&format!(
                "6 s later: queue index {cur:?} ({title}), engine token {:?}, position {:.2} s, state {:?}
",
                s.token, s.position, s.state
            ));
            text.push_str(if cur == Some(1) && s.position > 1.0 && s.position < 4.5 {
                "PASS: the app advanced to track 2 on its own; position continued from 0 without a restart
"
            } else {
                count_fail();
                "FAIL: did not land in track 2 as expected
"
            });
            write_report("gapless", &text);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

/// Edit theme.toml while running and check the window picks it up.
fn theme_reload() {
    fn count_near(img: &image::RgbaImage, rgb: [u8; 3]) -> usize {
        img.pixels()
            .filter(|p| p.0[..3].iter().zip(rgb).all(|(&a, b)| (i16::from(a) - i16::from(b)).abs() <= 6))
            .count()
    }
    let before: Rc<RefCell<Option<(image::RgbaImage, String)>>> = Rc::default();
    let b1 = before.clone();
    let steps: Vec<Step> = vec![
        (1500, Box::new(move |st, app| {
            let path = st.paths.theme();
            let original = std::fs::read_to_string(&path).unwrap_or_default();
            if let Some(img) = snapshot(app, "theme-before") {
                *b1.borrow_mut() = Some((img, original.clone()));
            }
            // Only the accent changes (pinned, so the art does not drive it); the rest of
            // the file stays as it was.
            let mut edited = original.replace("accent = \"#ffb347\"", "accent = \"#33ccff\"");
            if edited.contains("source = \"art\"") {
                edited = edited.replace("source = \"art\"", "source = \"fixed\"");
            } else {
                edited.push_str("
[accent]
source = \"fixed\"
transition_ms = 0
");
            }
            let _ = std::fs::write(&path, edited);
        })),
        (2000, Box::new(move |st, app| {
            let after = snapshot(app, "theme-after");
            let mut text = String::new();
            if let (Some(after), Some((before, original))) = (after, before.borrow_mut().take()) {
                let old = [0xff, 0xb3, 0x47];
                let new = [0x33, 0xcc, 0xff];
                text = format!(
                    "accent pixels before edit: old {} / new {}
accent pixels 2 s after saving theme.toml: old {} / new {}
",
                    count_near(&before, old),
                    count_near(&before, new),
                    count_near(&after, old),
                    count_near(&after, new)
                );
                verdict(&mut text, "theme.toml saved while running: the window takes the new accent", count_near(&after, new) > 500, count_near(&after, new));
                let _ = std::fs::write(st.paths.theme(), original);
            }
            write_report("theme", &text);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

fn tour() {
    let steps: Vec<Step> = vec![
        (2500, Box::new(|st, app| {
            views::go(st, Page::Home);
            let _ = app;
        })),
        (2500, Box::new(|_, app| {
            snapshot(app, "01-home");
        })),
        (100, Box::new(|_, app| app.global::<Nav>().set_sidebar_forced(true))),
        (800, Box::new(|_, app| {
            snapshot(app, "02-home-sidebar");
            app.global::<Nav>().set_sidebar_forced(false);
        })),
        (100, Box::new(|st, _| views::go(st, Page::Albums))),
        (2000, Box::new(|_, app| {
            snapshot(app, "03-albums");
        })),
        (100, Box::new(|st, _| {
            // A CJK-titled album if there is one.
            let id: Option<i64> = st
                .db
                .query_row(
                    "SELECT id FROM albums WHERE art IS NOT NULL AND (title GLOB '*[ぁ-んァ-ン一-龯]*' OR album_artist GLOB '*[ぁ-んァ-ン一-龯]*') ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if let Some(id) = id {
                views::open_album(st, id);
            }
        })),
        (2000, Box::new(|_, app| {
            snapshot(app, "04-album-cjk");
        })),
        (100, Box::new(|st, _| {
            let album = st.open_album.get();
            if let Some(id) = album {
                let tracks = library::query::album_tracks(&st.db, id).unwrap_or_default();
                playback::play_tracks(st, tracks, 0);
            }
        })),
        (2500, Box::new(|st, app| {
            views::go(st, Page::NowPlaying);
            let _ = app;
        })),
        (2000, Box::new(|_, app| {
            snapshot(app, "05-now-playing");
        })),
        (100, Box::new(|st, app| {
            views::go(st, Page::Search);
            app.global::<Search>().set_query("かめりあ".into());
            views::search(st, "かめりあ");
        })),
        (2000, Box::new(|_, app| {
            snapshot(app, "06-search-cjk");
        })),
        (100, Box::new(|st, _| {
            let artist: Option<String> = st
                .db
                .query_row("SELECT album_artist FROM albums GROUP BY album_artist ORDER BY COUNT(*) DESC LIMIT 1", [], |r| r.get(0))
                .ok();
            if let Some(a) = artist {
                views::open_artist(st, &a);
            }
        })),
        (2000, Box::new(|_, app| {
            snapshot(app, "07-artist");
        })),
        (100, Box::new(|_, app| app.global::<Nav>().set_queue_open(true))),
        (800, Box::new(|_, app| {
            snapshot(app, "08-queue");
            app.global::<Nav>().set_queue_open(false);
        })),
        (100, Box::new(|st, app| {
            // Playlist flow through the same callbacks the context menu uses.
            let a = app.global::<crate::Actions>();
            a.invoke_create_playlist("Bench playlist".into());
            let pid = library::query::playlists(&st.db).unwrap_or_default().into_iter().find(|p| p.name == "Bench playlist").map(|p| p.id);
            if let (Some(pid), Some(album)) = (pid, st.open_album.get()) {
                a.invoke_add_album_to_playlist(album as i32, pid as i32);
                views::open_playlist(st, pid);
            }
        })),
        (1500, Box::new(|st, app| {
            snapshot(app, "08b-playlist");
            if let Some(pid) = st.open_playlist.get() {
                let n = library::query::playlist_tracks(&st.db, pid).map(|t| t.len()).unwrap_or(0);
                write_report("playlist", &format!("playlist {pid}: {n} tracks after adding the open album
"));
                app.global::<crate::Actions>().invoke_delete_playlist(pid as i32);
            }
        })),
        (100, Box::new(|st, _| views::go(st, Page::Settings))),
        (1000, Box::new(|_, app| {
            snapshot(app, "09-settings");
        })),
        (100, Box::new(|st, _| views::go(st, Page::Shelves))),
        (1000, Box::new(|_, app| {
            snapshot(app, "10-shelves");
        })),
        (100, Box::new(|st, _| {
            let s = st.player.status();
            let mut text = format!(
                "playback status after tour: state {:?}, token {:?}, position {:.2} s, duration {:?}\n",
                s.state, s.token, s.position, s.duration
            );
            let cjk = st.open_album.get().and_then(|a| st.db.query_row("SELECT title FROM albums WHERE id = ?1", [a], |r| r.get::<_, String>(0)).ok()).unwrap_or_default();
            verdict(&mut text, "a CJK-titled album with art was opened and played", !cjk.is_ascii() && s.token.is_some(), &cjk);
            let found = st.app.upgrade().map(|a| a.global::<Search>().get_albums().row_count()).unwrap_or(0);
            verdict(&mut text, "search finds the CJK artist", found > 0, found);
            write_report("tour", &text);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

fn queue_line(st: &Rc<State>) -> String {
    let q = st.queue.borrow();
    let titles: Vec<String> = q.items().iter().map(|i| i.track.title.clone()).collect();
    format!(
        "queue [{}], current {:?} ({:?}), shuffled {}, repeat {:?}",
        titles.join(", "),
        q.current_index(),
        q.current().map(|i| i.track.title.clone()),
        q.shuffled(),
        q.repeat
    )
}

/// See `MP_BENCH=session-save` above.
fn session_save() {
    let report: Rc<RefCell<String>> = Rc::default();
    let (r1, r2) = (report.clone(), report.clone());
    let steps: Vec<Step> = vec![
        (1000, Box::new(move |st, _| {
            let album: Option<i64> = st.db.query_row("SELECT a.id FROM albums a WHERE (SELECT COUNT(*) FROM tracks t WHERE t.album_id = a.id AND t.duration >= 5) >= 3 ORDER BY a.title LIMIT 1", [], |r| r.get(0)).ok();
            let Some(album) = album else { return };
            let tracks = library::query::album_tracks(&st.db, album).unwrap_or_default();
            let first = tracks[0].clone();
            // History on the album's first track: two plays, a favourite, a playlist.
            library::query::record_play(&st.db, first.id).unwrap();
            library::query::record_play(&st.db, first.id).unwrap();
            library::query::toggle_favourite_track(&st.db, first.id).unwrap();
            let pl = library::query::create_playlist(&st.db, "Bench session").unwrap();
            library::query::add_to_playlist(&st.db, pl, &[first.id]).unwrap();
            playback::play_tracks(st, tracks, 1);
            playback::cycle_repeat(st); // all
            playback::set_shuffle(st, true);
            r1.borrow_mut().push_str(&format!("history on {:?} (id {}) at {}\n", first.title, first.id, first.path));
        })),
        (1500, Box::new(|st, _| playback::seek(st, 2.5))),
        (1200, Box::new(|st, _| playback::set_paused(st, true))),
        (500, Box::new(move |st, _| {
            let s = st.player.status();
            let mut r = r2.borrow_mut();
            r.push_str(&format!("{}\n", queue_line(st)));
            r.push_str(&format!("engine at exit: {:?}, position {:.2} s\n", s.state, s.position));
            r.push_str(&format!("window: {:?}\n", st.app.upgrade().and_then(|a| crate::placement::read(&a, st.hwnd.get()))));
            write_report("session-save", &r);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

/// See `MP_BENCH=session-check` above.
fn session_check() {
    let report: Rc<RefCell<String>> = Rc::default();
    let (r1, r2, r3, r4, r5, r6) = (report.clone(), report.clone(), report.clone(), report.clone(), report.clone(), report.clone());
    let steps: Vec<Step> = vec![
        (1000, Box::new(move |st, app| {
            let mut r = r1.borrow_mut();
            let s = st.player.status();
            let p = app.global::<crate::Player>();
            r.push_str(&format!("restored: {}\n", queue_line(st)));
            r.push_str(&format!("resume {:?}; engine {:?}, token {:?}\n", st.resume.get(), s.state, s.token));
            r.push_str(&format!(
                "player bar: has-track {}, title {:?}, position {:.2} ({}/{}), playing {}, shuffle {}, repeat {}\n",
                p.get_has_track(),
                p.get_title().to_string(),
                p.get_position(),
                p.get_position_text(),
                p.get_duration_text(),
                p.get_playing(),
                p.get_shuffle(),
                p.get_repeat()
            ));
            r.push_str(&format!("window now {:?}; saved {:?}\n", st.app.upgrade().and_then(|a| crate::placement::read(&a, st.hwnd.get())), st.settings.borrow().window));
            {
                let q = st.queue.borrow();
                let detail = format!("{} items, current {:?}", q.items().len(), q.current_index());
                verdict(&mut r, "the queue and its current track come back", q.items().len() >= 3 && q.current().is_some(), detail);
                verdict(&mut r, "shuffle and repeat come back", q.shuffled() && p.get_repeat() == 1, format!("shuffle {}, repeat {}", q.shuffled(), p.get_repeat()));
            }
            verdict(
                &mut r,
                "the position comes back, paused",
                (2.0..=5.0).contains(&p.get_position()) && !p.get_playing() && s.state == player_core::PlayState::Stopped,
                format!("{:.2} s", p.get_position()),
            );
            // The history of the track that had it (found by its playlist).
            let pl = library::query::playlists(&st.db).unwrap_or_default().into_iter().find(|p| p.name == "Bench session");
            if let Some(pl) = pl {
                for t in library::query::playlist_tracks(&st.db, pl.id).unwrap_or_default() {
                    let plays = library::query::play_count(&st.db, t.id).unwrap_or(-1);
                    let fav = library::query::is_favourite_track(&st.db, t.id).unwrap_or(false);
                    let moved = std::env::var_os("MP_BENCH_MOVED").map(|m| t.path.contains(&*m.to_string_lossy())).unwrap_or(true);
                    verdict(&mut r, "the track's plays, favourite and playlist entry survive (its folder moved)", plays == 2 && fav && moved, format!("plays {plays}, favourite {fav}, at {}", t.path));
                    r.push_str(&format!(
                        "playlist entry: {:?} (id {}) at {}; plays {}, favourite {}\n",
                        t.title,
                        t.id,
                        t.path,
                        library::query::play_count(&st.db, t.id).unwrap_or(-1),
                        library::query::is_favourite_track(&st.db, t.id).unwrap_or(false)
                    ));
                }
            } else {
                r.push_str("playlist missing\n");
                verdict(&mut r, "the playlist survives", false, "missing");
            }
            snapshot(app, "session-restored");
        })),
        (100, Box::new(|st, _| playback::toggle_play(st))),
        (1500, Box::new(move |st, _| {
            let s = st.player.status();
            r2.borrow_mut().push_str(&format!("after play: {:?}, position {:.2} s, {}\n", s.state, s.position, queue_line(st)));
            playback::set_paused(st, true);
            // Typing "alpha" fast, then a query that matches nothing, then "beta": only
            // the last one's results may show.
            for q in ["a", "al", "alp", "alph", "alpha", "zzzz-nothing", "beta"] {
                views::search(st, q);
            }
        })),
        (1000, Box::new(move |_, app| {
            let s = app.global::<Search>();
            let rows = s.get_tracks();
            let tracks: Vec<String> = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).map(|t| t.title.to_string()).collect();
            r3.borrow_mut().push_str(&format!("search after typing ...beta: {} albums, tracks {:?}\n", s.get_albums().row_count(), tracks));
            verdict(&mut r3.borrow_mut(), "only the last query's results show", !tracks.is_empty() && tracks.iter().all(|t| t.to_lowercase().contains("beta")), format!("{tracks:?}"));
        })),
        (100, Box::new(move |st, _| {
            let Some(extra) = std::env::var_os("MP_BENCH_EXTRA").map(PathBuf::from) else { return };
            r4.borrow_mut().push_str(&format!("counts before adding extra: {:?}\n", library::query::counts(&st.db)));
            crate::scanning::add_folder(st, extra);
        })),
        (3000, Box::new(move |st, _| {
            r5.borrow_mut().push_str(&format!("counts with extra: {:?}\n", library::query::counts(&st.db)));
            EXTRA_COUNTS.with(|c| c.set(library::query::counts(&st.db).map(|c| c.1).unwrap_or(0)));
            // Remove it while a scan runs: the removal waits for the scan.
            crate::scanning::rescan(st);
            let n = st.settings.borrow().folders.len();
            if n > 1 {
                let t = Instant::now();
                crate::scanning::remove_folder(st, n - 1);
                r5.borrow_mut().push_str(&format!(
                    "remove_folder returned in {:.1} ms (scanning {}, pending {})\n",
                    t.elapsed().as_secs_f64() * 1000.0,
                    st.scanning.get(),
                    st.pending_forget.borrow().len()
                ));
            }
        })),
        (3000, Box::new(move |st, app| {
            let mut r = r6.borrow_mut();
            r.push_str(&format!("counts after removing: {:?}; scanning {}\n", library::query::counts(&st.db), st.scanning.get()));
            if std::env::var_os("MP_BENCH_EXTRA").is_some() {
                let (with, after) = (EXTRA_COUNTS.with(Cell::get), library::query::counts(&st.db).map(|c| c.1).unwrap_or(0));
                verdict(&mut r, "a second folder is added, then removed while a scan runs", with > after && after > 0 && !st.scanning.get(), format!("{with} tracks with it, {after} after"));
            }
            r.push_str(&format!("status: {}\n", app.global::<crate::Library>().get_status()));
            r.push_str(&format!("{}\n", queue_line(st)));
            write_report("session-check", &r);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

/// `MP_BENCH=guard`: what the mass-removal guard shows in Settings after the startup
/// scan (snapshot), then the confirm button's action, and the counts after it.
fn guard() {
    let report: Rc<RefCell<String>> = Rc::default();
    let (r1, r2) = (report.clone(), report.clone());
    let steps: Vec<Step> = vec![
        (500, Box::new(move |st, app| {
            views::go(st, Page::Settings);
            let l = app.global::<crate::Library>();
            r1.borrow_mut().push_str(&format!(
                "after the startup scan: counts {:?}\nstatus: {}\nheld: {:?}\n",
                library::query::counts(&st.db),
                l.get_status(),
                l.get_held().to_string()
            ));
            let held = !l.get_held().is_empty();
            let tracks = library::query::counts(&st.db).map(|c| c.1).unwrap_or(0);
            GUARD_BEFORE.with(|g| g.set(tracks));
            verdict(&mut r1.borrow_mut(), "nine in ten files gone: the tracks are held back, not removed", held && tracks > 100, format!("held {held}, {tracks} tracks"));
        })),
        (1200, Box::new(|st, app| {
            snapshot(app, "guard-settings");
            if !app.global::<crate::Library>().get_held().is_empty() {
                app.global::<crate::Actions>().invoke_confirm_removal();
            }
            let _ = st;
        })),
        (3000, Box::new(move |st, app| {
            let mut r = r2.borrow_mut();
            let l = app.global::<crate::Library>();
            r.push_str(&format!("after confirm: counts {:?}\nstatus: {}\nheld: {:?}\n", library::query::counts(&st.db), l.get_status(), l.get_held().to_string()));
            let after = library::query::counts(&st.db).map(|c| c.1).unwrap_or(0);
            let before = GUARD_BEFORE.with(Cell::get);
            verdict(&mut r, "Remove missing tracks removes them", after < before / 5 && l.get_held().is_empty(), format!("{before} -> {after} tracks"));
            write_report("guard", &r);
            let _ = slint::quit_event_loop();
        })),
    ];
    run_steps(steps);
}

/// See `MP_BENCH=idle-paused` / `idle-stopped` above.
fn idle(paused: bool) {
    let secs: u64 = std::env::var("MP_BENCH_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
    let mut steps: Vec<Step> = Vec::new();
    if paused {
        steps.push((500, Box::new(|st, _| {
            let album: Option<i64> = st.db.query_row("SELECT a.id FROM albums a WHERE (SELECT MIN(t.duration) FROM tracks t WHERE t.album_id = a.id) >= 10 ORDER BY a.title LIMIT 1", [], |r| r.get(0)).ok();
            let tracks = album.map(|a| library::query::album_tracks(&st.db, a).unwrap_or_default()).unwrap_or_default();
            playback::play_tracks(st, tracks, 0);
        })));
        steps.push((3000, Box::new(|st, _| playback::set_paused(st, true))));
    }
    steps.push((1000, Box::new(move |st, app| {
        let _ = app.hide();
        let state = st.player.status().state;
        let mut text = format!("idle {}: engine {state:?}, window hidden; quitting in {secs} s\n", if paused { "paused" } else { "stopped" });
        let want = if paused { player_core::PlayState::Paused } else { player_core::PlayState::Stopped };
        verdict(&mut text, "the engine is in the state the idle run measures", state == want, format!("{state:?}"));
        write_report(if paused { "idle-paused" } else { "idle-stopped" }, &text);
    })));
    steps.push((secs * 1000, Box::new(|_, _| {
        let _ = slint::quit_event_loop();
    })));
    run_steps(steps);
}

/// The grid rows on screen at this scroll position, as album indices. Mirrors
/// `AlbumGridView`'s layout in pages/browse.slint.
fn grid_visible(app: &AppWindow, count: usize) -> std::ops::Range<usize> {
    let theme = app.global::<crate::Theme>();
    let size = app.window().size().to_logical(app.window().scale_factor());
    let (cover, margin, top) = (theme.get_cover_size(), theme.get_page_margin(), theme.get_page_top());
    let gap = 24.0;
    let inner = size.width - 2.0 * margin;
    let cols = ((inner + gap) / (cover + gap)).floor().max(1.0) as usize;
    let row_h = cover + 50.0 + gap;
    let viewport = size.height - (top + 56.0);
    let y = -app.global::<Grid>().get_content_y();
    let first = (y / row_h).floor().max(0.0) as usize;
    let last = ((y + viewport) / row_h).floor() as usize;
    (first * cols).min(count)..((last + 1) * cols).min(count)
}

/// How far the album grid can scroll (px), from the same layout as `grid_visible`.
fn grid_max_scroll(app: &AppWindow, count: usize) -> f32 {
    let theme = app.global::<crate::Theme>();
    let size = app.window().size().to_logical(app.window().scale_factor());
    let (cover, margin, top) = (theme.get_cover_size(), theme.get_page_margin(), theme.get_page_top());
    let gap = 24.0;
    let cols = ((size.width - 2.0 * margin + gap) / (cover + gap)).floor().max(1.0) as usize;
    let rows = count.div_ceil(cols);
    (rows as f32 * (cover + 50.0 + gap) - (size.height - (top + 56.0))).max(0.0)
}

/// See `MP_BENCH=covers` above.
fn covers_after_flick() {
    let scroller = Rc::new(Timer::default());
    let poller = Rc::new(Timer::default());
    let s1 = scroller.clone();
    run_steps(vec![
        (1500, Box::new(|st, _| views::go(st, Page::Albums))),
        (3000, Box::new(move |_, app| {
            let weak = app.as_weak();
            let start = Instant::now();
            let decodes_at_start = crate::covers::decodes();
            let s2 = s1.clone();
            let poller = poller.clone();
            // Up to 40,000 px/s for one second: a hard flick through ~150 rows (or to near the end
            // of a smaller grid, so that covers are on screen when it stops).
            let n = app.global::<Grid>().get_count().max(0) as usize;
            let distance = (grid_max_scroll(app, n) - 400.0).clamp(0.0, 40_000.0);
            s1.start(TimerMode::Repeated, Duration::from_millis(1), move || {
                let Some(app) = weak.upgrade() else { return };
                let t = start.elapsed().as_secs_f32().min(1.0);
                app.global::<Grid>().set_content_y(-(t * distance));
                if t < 1.0 {
                    return;
                }
                s2.stop();
                let stopped = Instant::now();
                let decodes_at_stop = crate::covers::decodes();
                let weak = app.as_weak();
                let p2 = poller.clone();
                let done: Rc<RefCell<Option<(Duration, usize)>>> = Rc::default();
                poller.start(TimerMode::Repeated, Duration::from_millis(5), move || {
                    let Some(app) = weak.upgrade() else { return };
                    let n = app.global::<Grid>().get_count().max(0) as usize;
                    if done.borrow().is_none() {
                        let missing = state::with(|st| {
                            grid_visible(&app, n)
                                .filter(|&i| {
                                    st.models.grid.art_at(i).flatten().is_some_and(|stem| !crate::covers::cached(&stem, crate::covers::Kind::Thumb))
                                })
                                .count()
                        })
                        .unwrap_or(0);
                        if missing == 0 || stopped.elapsed() > Duration::from_secs(30) {
                            *done.borrow_mut() = Some((stopped.elapsed(), crate::covers::decodes()));
                            snapshot(&app, "covers-after-flick");
                        }
                        return;
                    }
                    if stopped.elapsed() < Duration::from_secs(8) {
                        return;
                    }
                    p2.stop();
                    let (visible_after, decodes_visible) = done.borrow().unwrap();
                    let range = grid_visible(&app, n);
                    let mut text = format!(
                        "albums in grid: {n}; on screen after the flick: {} (albums {}..{})\n\
                         decodes during the flick: {}\n\
                         every cover on screen in memory {:.0} ms after the flick stopped ({} more decodes by then)\n\
                         decodes in the 8 s after the flick: {}\n",
                        range.len(),
                        range.start,
                        range.end,
                        decodes_at_stop - decodes_at_start,
                        visible_after.as_secs_f64() * 1000.0,
                        decodes_visible - decodes_at_stop,
                        crate::covers::decodes() - decodes_at_stop,
                    );
                    verdict(&mut text, "covers are on screen after the flick", !range.is_empty(), range.len());
                    verdict(&mut text, "every cover on screen is in memory within 3 s of the flick stopping", visible_after < Duration::from_secs(3), format!("{:.0} ms", visible_after.as_secs_f64() * 1000.0));
                    playback_covers(text);
                });
            });
        })),
    ]);
}

/// The player bar only needs a thumbnail. Opening Now Playing requests the large
/// image, and fast skips must leave both images showing the current track.
fn playback_covers(text: String) {
    use crate::covers::{self, Kind};
    use crate::Player;

    fn current_image(app: &AppWindow, kind: Kind) -> bool {
        let p = app.global::<Player>();
        let stem = p.get_cover_stem();
        let displayed = if kind == Kind::Full { p.get_full() } else { p.get_cover() };
        !stem.is_empty() && covers::cached(&stem, kind) && covers::get(&stem, kind) == Some(displayed)
    }

    let log = Rc::new(RefCell::new(text));
    let (bar, full, skipped) = (log.clone(), log.clone(), log.clone());
    run_steps(vec![
        (0, Box::new(|st, _| {
            // Two distinct covers from the fixture library.
            let mut seen = std::collections::HashSet::new();
            let tracks = library::query::all_albums(&st.db, library::query::AlbumSort::Artist, false)
                .unwrap_or_default().into_iter().filter(|a| a.art.as_ref().is_some_and(|s| seen.insert(s.clone())))
                .filter_map(|a| library::query::album_tracks(&st.db, a.id).ok()?.into_iter().next()).take(2).collect();
            playback::play_tracks(st, tracks, 0);
        })),
        (400, Box::new(move |st, app| {
            let p = app.global::<Player>();
            let stem = p.get_cover_stem();
            let ok = current_image(app, Kind::Thumb) && !covers::cached(&stem, Kind::Full) && p.get_full().size().width == 0;
            verdict(&mut bar.borrow_mut(), "the player bar loads only its thumbnail", ok, stem);
            views::go(st, Page::NowPlaying);
        })),
        (500, Box::new(move |st, app| {
            verdict(&mut full.borrow_mut(), "Now Playing loads the current full cover on entry", current_image(app, Kind::Full), app.get_full_cover_stem());
            playback::queue_play(st, 1);
            playback::queue_play(st, 0);
            playback::queue_play(st, 1);
        })),
        (500, Box::new(move |st, app| {
            let ok = st.queue.borrow().current_index() == Some(1) && current_image(app, Kind::Thumb) && current_image(app, Kind::Full);
            verdict(&mut skipped.borrow_mut(), "both covers follow rapid skips", ok, app.get_full_cover_stem());
            #[cfg(target_os = "macos")]
            {
                let path = st.art.paths(&app.get_full_cover_stem()).full;
                let matches = crate::platform::media::artwork_matches;
                verdict(&mut skipped.borrow_mut(), "macOS Now Playing serves the current album after skips", matches(Some(&path)), path.display());
                crate::media::update_metadata(st, None);
                verdict(&mut skipped.borrow_mut(), "clearing metadata releases the native cover", matches(None), "empty Now Playing artwork");
            }
            views::go(st, Page::Albums);
        })),
        (200, Box::new(move |_, app| {
            let ok = app.global::<Player>().get_full().size().width == 0;
            verdict(&mut log.borrow_mut(), "leaving Now Playing releases its displayed full cover", ok, app.get_full_cover_stem());
            write_report("covers", &log.borrow());
            let _ = slint::quit_event_loop();
        })),
    ]);
}

/// See `MP_BENCH=playlist` above.
fn big_playlist(frames: Rc<RefCell<Frames>>) {
    let report: Rc<RefCell<String>> = Rc::default();
    let playlist: Rc<std::cell::Cell<i64>> = Rc::default();
    let opened: Rc<std::cell::Cell<Option<Instant>>> = Rc::default();
    let (r1, r2, r3, r4) = (report.clone(), report.clone(), report.clone(), report.clone());
    let (p1, p2, p3) = (playlist.clone(), playlist.clone(), playlist.clone());
    let (f1, f2, f3, f4) = (frames.clone(), frames.clone(), frames.clone(), frames.clone());
    let (o1, o2, o3, o4) = (opened.clone(), opened.clone(), opened.clone(), opened.clone());
    fn first_frame(f: &Rc<RefCell<Frames>>, at: Instant) -> String {
        let data = stop_recording(f);
        match data.iter().find(|(t, _)| *t >= at) {
            Some((t, render)) => format!(
                "first frame {:.1} ms after the call (its render {:.1} ms)",
                (*t - at).as_secs_f64() * 1000.0,
                render.as_secs_f64() * 1000.0
            ),
            None => "no frame".to_owned(),
        }
    }
    run_steps(vec![
        (500, Box::new(move |st, _| {
            let ids: Vec<i64> = st
                .db
                .prepare("SELECT id FROM tracks ORDER BY id LIMIT 3000")
                .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                .unwrap_or_default();
            let pl = library::query::create_playlist(&st.db, "Bench 3000").unwrap();
            library::query::add_to_playlist(&st.db, pl, &ids).unwrap();
            views::refresh_playlists(st);
            p1.set(pl);
            r1.borrow_mut().push_str(&format!("playlist of {} tracks\n", ids.len()));
        })),
        (1000, Box::new(move |st, _| {
            start_recording(&f1);
            let t0 = Instant::now();
            o1.set(Some(t0));
            views::open_playlist(st, p2.get());
            r2.borrow_mut().push_str(&format!("open: filled in {:.1} ms; ", t0.elapsed().as_secs_f64() * 1000.0));
        })),
        (2000, Box::new(move |_, app| {
            let line = first_frame(&f2, o2.get().unwrap());
            r3.borrow_mut().push_str(&format!("{line}\n"));
            snapshot(app, "playlist-top");
            app.global::<Nav>().set_page_scroll(-60_000.0);
        })),
        (1000, Box::new(move |st, app| {
            let y = app.global::<Nav>().get_page_scroll();
            snapshot(app, "playlist-deep");
            r4.borrow_mut().push_str(&format!("scrolled to {y:.0} px\n"));
            views::go(st, Page::Home);
        })),
        (1500, Box::new(move |st, _| {
            start_recording(&f3);
            o3.set(Some(Instant::now()));
            views::back(st);
        })),
        (2000, Box::new(move |st, app| {
            let line = first_frame(&f4, o4.get().unwrap());
            let y = app.global::<Nav>().get_page_scroll();
            snapshot(app, "playlist-back");
            let mut r = report.borrow_mut();
            r.push_str(&format!("back to it: {line}; scroll {y:.0} px (was -60000)\n"));
            r.push_str(&format!(
                "page {:?}, the playlist open: {}\n",
                app.global::<Nav>().get_page(),
                st.open_playlist.get() == Some(p3.get())
            ));
            let n = library::query::playlist_tracks(&st.db, p3.get()).map(|t| t.len()).unwrap_or(0);
            verdict(&mut r, "the playlist holds every track added", n >= 2_000, format!("{n} tracks"));
            verdict(&mut r, "Back returns to the playlist", st.open_playlist.get() == Some(p3.get()), format!("{:?}", app.global::<Nav>().get_page()));
            verdict(&mut r, "its scroll position comes back", (y - -60_000.0).abs() < 1.0 || y < -20_000.0, format!("{y:.0} px"));
            // Then shuffle and name clashes (`bench/playlist.rs`), then the selection of track lists
            // (`bench/selection.rs`), which writes the report and quits.
            crate::bench::playlist::run(std::mem::take(&mut *r), p3.get());
        })),
    ]);
}

// ---- phase4 -----------------------------------------------------------------------

/// A key press through Slint's own event dispatch, as the keyboard would deliver it:
/// modifiers down, the key down and up, modifiers up. No OS input is sent.
pub(crate) fn press(app: &AppWindow, key: slint::platform::Key, mods: &[slint::platform::Key]) {
    use slint::platform::WindowEvent;
    let w = app.window();
    for m in mods {
        w.dispatch_event(WindowEvent::KeyPressed { text: (*m).into() });
    }
    w.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    for m in mods.iter().rev() {
        w.dispatch_event(WindowEvent::KeyReleased { text: (*m).into() });
    }
}

/// Characters typed (letters, a space, "/").
pub(crate) fn type_text(app: &AppWindow, text: &str) {
    use slint::platform::WindowEvent;
    for c in text.chars() {
        let s: slint::SharedString = c.to_string().into();
        app.window().dispatch_event(WindowEvent::KeyPressed { text: s.clone() });
        app.window().dispatch_event(WindowEvent::KeyReleased { text: s });
    }
}

pub(crate) fn tabs(app: &AppWindow, n: usize) {
    for _ in 0..n {
        press(app, slint::platform::Key::Tab, &[]);
    }
}

/// Columns of the Albums grid at this window size (as `AlbumGridView` lays them out).
fn grid_cols(app: &AppWindow) -> usize {
    let theme = app.global::<crate::Theme>();
    let size = app.window().size().to_logical(app.window().scale_factor());
    let inner = size.width - 2.0 * theme.get_page_margin();
    ((inner + 24.0) / (theme.get_cover_size() + 24.0)).floor().max(1.0) as usize
}

fn line(r: &Rc<RefCell<String>>, text: String) {
    log::info!("phase4: {text}");
    r.borrow_mut().push_str(&text);
    r.borrow_mut().push('\n');
}

fn queue_titles(st: &State) -> Vec<String> {
    st.queue.borrow().items().iter().map(|i| i.track.title.clone()).collect()
}

fn playlist_titles(st: &State, id: i64) -> Vec<String> {
    library::query::playlist_tracks(&st.db, id).unwrap_or_default().into_iter().map(|t| t.title).collect()
}

fn album_id(st: &State, title: &str) -> Option<i64> {
    st.db.query_row("SELECT id FROM albums WHERE title = ?1", [title], |r| r.get(0)).ok()
}

pub(crate) fn menu_labels(app: &AppWindow) -> Vec<String> {
    let m = app.global::<crate::MenuStore>();
    (0..m.get_items().row_count()).filter_map(|i| m.get_items().row_data(i)).map(|i| i.label.to_string()).collect()
}

fn st_paused(_app: &AppWindow) -> bool {
    crate::state::with(|st| st.player.is_paused()).unwrap_or(false)
}

/// See `MP_BENCH=phase4` above.
fn phase4() {
    use slint::platform::Key;
    let r: Rc<RefCell<String>> = Rc::default();
    let pl: Rc<std::cell::Cell<i64>> = Rc::default();
    let mut steps: Vec<Step> = Vec::new();
    // Each step gets the report and the playlist id.
    type StepFn = Box<dyn Fn(&Rc<State>, &AppWindow, &Rc<RefCell<String>>, &Rc<std::cell::Cell<i64>>)>;
    let mut step = |ms: u64, f: StepFn| {
        let (r, pl) = (r.clone(), pl.clone());
        steps.push((ms, Box::new(move |st: &Rc<State>, app: &AppWindow| f(st, app, &r, &pl))));
    };
    step(500, Box::new(|st, app, r, _| {
        let size = app.window().size();
        line(r, format!("window {}x{} physical, scale {}", size.width, size.height, app.window().scale_factor()));
        line(r, format!("library: {:?} (albums, tracks)", library::query::counts(&st.db)));
        line(r, format!("scan status: {}", app.global::<crate::Library>().get_status()));
        views::go(st, Page::Albums);
    }));
    step(1500, Box::new(|st, app, r, _| {
        snapshot(app, "p4-01-albums");
        app.global::<Grid>().set_filter("night".into());
        views::filter_grid(st, "night");
        let g = app.global::<Grid>();
        line(r, format!("filter \"night\": {} of {} albums", g.get_count(), g.get_total()));
        verdict(&mut r.borrow_mut(), "the filter narrows the grid", g.get_count() > 0 && g.get_count() < g.get_total(), g.get_count());
    }));
    step(1200, Box::new(|st, app, r, _| {
        snapshot(app, "p4-02-albums-filter");
        let g = app.global::<Grid>();
        g.set_filter("dvorak".into());
        views::filter_grid(st, "dvorak");
        line(r, format!("filter \"dvorak\" (tagged Dvořák): {} albums", g.get_count()));
        verdict(&mut r.borrow_mut(), "the filter folds accents (dvorak finds Dvořák)", g.get_count() == 1, g.get_count());
        g.set_filter("".into());
        views::filter_grid(st, "");
        // The sort menu, under its button (top right of the header).
        let size = app.window().size().to_logical(app.window().scale_factor());
        crate::menus::request_sort(st, size.width - 340.0, 124.0);
    }));
    step(800, Box::new(|st, app, r, _| {
        snapshot(app, "p4-03-sort-menu");
        line(r, format!("sort menu: {:?}", menu_labels(app)));
        crate::menus::activate(st, 2, -1); // Release date
    }));
    step(1200, Box::new(|st, app, r, _| {
        let g = app.global::<Grid>();
        let first = |g: &Grid| -> Vec<String> { (0..3).filter_map(|i| g.get_albums().row_data(i)).map(|a| format!("{} ({})", a.title, a.year)).collect() };
        line(r, format!("sorted by {} {}: first {:?}", g.get_sort_label(), if g.get_descending() { "desc" } else { "asc" }, first(&g)));
        snapshot(app, "p4-04-albums-released");
        views::flip_grid(st);
        line(r, format!("flipped to {}: first {:?}", if g.get_descending() { "desc" } else { "asc" }, first(&g)));
        line(r, format!("saved in settings: {:?} desc {}", st.settings.borrow().albums_sort, st.settings.borrow().albums_desc));
        views::sort_grid(st, library::query::AlbumSort::Artist);
        views::go(st, Page::Artists);
    }));
    step(1500, Box::new(|_st, app, r, _| {
        let b = app.global::<crate::Browse>();
        let names: Vec<String> =
            (0..b.get_artists_count().min(14) as usize).filter_map(|i| b.get_artists().row_data(i)).map(|a| format!("{} [{}]", a.title, a.artist)).collect();
        line(r, format!("artists: {} {names:?}...", b.get_artists_count()));
        snapshot(app, "p4-05-artists");
    }));
    step(100, Box::new(|st, _app, _, _| views::go(st, Page::Genres)));
    step(1500, Box::new(|_st, app, r, _| {
        let b = app.global::<crate::Browse>();
        let names: Vec<String> = (0..b.get_groups_count() as usize).filter_map(|i| b.get_groups().row_data(i)).map(|a| format!("{} [{}]", a.title, a.artist)).collect();
        line(r, format!("genres: {names:?}"));
        snapshot(app, "p4-06-genres");
    }));
    step(100, Box::new(|st, _app, _, _| views::go(st, Page::Labels)));
    step(1500, Box::new(|st, app, r, _| {
        let b = app.global::<crate::Browse>();
        let names: Vec<String> = (0..b.get_groups_count() as usize).filter_map(|i| b.get_groups().row_data(i)).map(|a| a.title.to_string()).collect();
        line(r, format!("labels: {names:?}"));
        snapshot(app, "p4-07-labels");
        views::open_group(st, "Warp");
    }));
    step(1500, Box::new(|_st, app, r, _| {
        let p = app.global::<crate::ShelfPage>();
        let nav = app.global::<Nav>();
        line(r, format!("label Warp: page {:?} {:?}, {} albums, pill {:?}", nav.get_page(), p.get_title().to_string(), p.get_count(), nav.get_title().to_string()));
        verdict(&mut r.borrow_mut(), "a label opens its albums", p.get_count() == 2, p.get_count());
        snapshot(app, "p4-08-label-albums");
    }));
    step(100, Box::new(|st, app, _, _| {
        views::go(st, Page::Search);
        app.global::<Search>().set_query("beyonce".into());
        views::search(st, "beyonce");
    }));
    step(1500, Box::new(|_st, app, r, _| {
        let s = app.global::<Search>();
        let artists: Vec<String> = (0..s.get_artists().row_count()).filter_map(|i| s.get_artists().row_data(i)).map(|a| a.text.to_string()).collect();
        line(r, format!("search beyonce: {} albums, artists {artists:?}, {} tracks", s.get_albums().row_count(), s.get_tracks().row_count()));
        snapshot(app, "p4-09-search-accents");
    }));
    step(100, Box::new(|st, app, _, _| {
        app.global::<Search>().set_query("bulk".into());
        views::search(st, "bulk");
    }));
    step(1500, Box::new(|_st, app, r, _| {
        let s = app.global::<Search>();
        line(r, format!("search bulk: more albums {}, artists {}, tracks {}", s.get_more_albums(), s.get_more_artists(), s.get_more_tracks()));
        verdict(&mut r.borrow_mut(), "search offers See all for albums, artists and tracks", s.get_more_albums() && s.get_more_artists() && s.get_more_tracks(), "");
        snapshot(app, "p4-10-search-see-all");
        app.global::<crate::Actions>().invoke_search_all("tracks".into());
    }));
    step(1500, Box::new(|st, app, r, _| {
        let t = app.global::<crate::TrackPage>();
        line(r, format!("see all tracks: page {:?}, {:?}, {:?}, {} rows", app.global::<Nav>().get_page(), t.get_title().to_string(), t.get_info().to_string(), t.get_tracks().row_count()));
        snapshot(app, "p4-11-search-all-tracks");
        views::back(st);
        app.global::<crate::Actions>().invoke_search_all("artists".into());
    }));
    step(1500, Box::new(|st, app, r, _| {
        let b = app.global::<crate::Browse>();
        line(r, format!("see all artists: {:?}, {} artists", b.get_artists_title().to_string(), b.get_artists_count()));
        snapshot(app, "p4-12-search-all-artists");
        views::back(st);
        app.global::<Search>().set_query("kendrick".into());
        views::search(st, "kendrick");
    }));
    step(1200, Box::new(|_st, app, r, _| {
        let s = app.global::<Search>();
        let artists: Vec<String> = (0..s.get_artists().row_count()).filter_map(|i| s.get_artists().row_data(i)).map(|a| a.text.to_string()).collect();
        line(r, format!("search kendrick (only featured on a track): artists {artists:?}"));
        verdict(&mut r.borrow_mut(), "an artist credited only in a title is found", artists.iter().any(|a| a.contains("Kendrick")), format!("{artists:?}"));
        snapshot(app, "p4-13-search-featured");
    }));
    // Playlists: one made by a drop of Lemonade, a reorder, the track menu there.
    step(100, Box::new(|st, _app, r, pl| {
        let id = library::query::create_playlist(&st.db, "Bench mix").unwrap();
        pl.set(id);
        views::refresh_playlists(st);
        if let Some(a) = album_id(st, "Lemonade") {
            views::drop_on_playlist(st, crate::dnd::Payload::Album(a), id);
        }
        line(r, format!("after dropping Lemonade on it: {:?}", playlist_titles(st, id)));
        verdict(&mut r.borrow_mut(), "an album dropped on a playlist adds its tracks", playlist_titles(st, id).len() == 6, playlist_titles(st, id).len());
        views::open_playlist(st, id);
    }));
    step(800, Box::new(|st, app, r, pl| {
        snapshot(app, "p4-14-playlist");
        views::move_in_playlist(st, 0, 3); // the first row to the line before the 4th
        line(r, format!("dragged row 0 to the line before row 3: {:?}", playlist_titles(st, pl.get())));
        verdict(&mut r.borrow_mut(), "a drag reorders the playlist", playlist_titles(st, pl.get()).first().map(String::as_str) == Some("Hold Up"), "");
        let size = app.window().size().to_logical(app.window().scale_factor());
        crate::menus::request(st, "track", "playlist", 1, 0, size.width / 2.0, 380.0);
    }));
    step(800, Box::new(|st, app, r, _| {
        snapshot(app, "p4-15-playlist-track-menu");
        line(r, format!("playlist track menu: {:?}", menu_labels(app)));
        crate::menus::key(st, "escape");
        app.global::<Nav>().set_sidebar_forced(true);
    }));
    step(800, Box::new(|_st, app, _, _| {
        snapshot(app, "p4-16-sidebar");
        let l = app.global::<crate::Library>();
        l.set_scanning(true);
        l.set_status("Reading tags 1,204 / 4,816".into());
    }));
    step(600, Box::new(|_st, app, _, _| {
        snapshot(app, "p4-17-sidebar-scanning");
        app.global::<crate::Library>().set_scanning(false);
        app.global::<crate::Library>().set_status("250 albums, 273 tracks (status restored by the bench)".into());
        app.global::<Nav>().set_sidebar_forced(false);
    }));
    step(600, Box::new(|st, app, r, _| {
        let album = album_id(st, "Lemonade");
        crate::menus::request(st, "album", "", 0, album.unwrap_or(0) as i32, 400.0, 300.0);
        line(r, format!("album menu: {:?}", menu_labels(app)));
        let folder = album.and_then(|a| library::query::album_folder(&st.db, a).ok().flatten()).unwrap_or_default();
        line(r, format!("Show in Explorer would run (not run here): {}", crate::platform::shell::describe_show(&folder)));
    }));
    step(500, Box::new(|st, app, _, _| {
        snapshot(app, "p4-15b-album-menu");
        crate::menus::key(st, "escape");
        views::go(st, Page::Settings);
    }));
    step(1200, Box::new(|_st, app, r, _| {
        let u: Vec<String> = app.global::<crate::Library>().get_unreadable().iter().map(|s| s.to_string()).collect();
        line(r, format!("unreadable files listed: {u:?}"));
        verdict(&mut r.borrow_mut(), "Settings lists the unreadable files", u.len() == 3, u.len());
        snapshot(app, "p4-18-settings");
        views::settings_section(_st, crate::settings::nav::PLAYBACK);
    }));
    step(800, Box::new(|st, app, _, _| {
        snapshot(app, "p4-19-settings-playback");
        views::settings_section(st, crate::settings::nav::KEYS);
    }));
    step(800, Box::new(|st, app, _, _| {
        snapshot(app, "p4-20-settings-keys");
        views::settings_section(st, crate::settings::nav::PLAYBACK);
        let a = app.global::<crate::Actions>();
        a.invoke_toggle_prevent_clipping();
        a.invoke_toggle_close_action();
    }));
    step(800, Box::new(|st, app, r, _| {
        // The two new settings, toggled through their buttons' callbacks: the page, the
        // settings and the engine's gain config follow; then back to the defaults.
        let p = app.global::<crate::Player>();
        let s = st.settings.borrow().clone();
        line(r, format!(
            "toggled: clipping protection shown {} (settings {}), close button shows {:?} (settings {:?})",
            p.get_prevent_clipping(),
            s.prevent_clipping,
            if p.get_close_quits() { "Quit" } else { "Hide to tray" },
            s.close_action
        ));
        snapshot(app, "p4-20b-settings-toggled");
        let a = app.global::<crate::Actions>();
        a.invoke_toggle_prevent_clipping();
        a.invoke_toggle_close_action();
        let s = st.settings.borrow().clone();
        line(r, format!("  toggled back: clipping {}, close {:?}", s.prevent_clipping, s.close_action));
        views::settings_section(st, crate::settings::nav::LIBRARY);
    }));
    // ---- keyboard -----------------------------------------------------------------
    step(500, Box::new(|_st, app, _, _| {
        // Settings > Library: Tab to "Show 2 unreadable files" (after the filter box, the
        // section list and Rescan) and press Enter.
        app.invoke_focus_shortcuts();
        tabs(app, 4);
    }));
    step(500, Box::new(|_st, app, _, _| {
        snapshot(app, "p4-21-key-settings-tab4");
        press(app, Key::Return, &[]);
    }));
    step(800, Box::new(|_st, app, r, _| {
        snapshot(app, "p4-22-key-settings-unreadable-shown");
        line(r, "settings: Tab x4, Enter (p4-22 should show the list open)".into());
    }));
    step(100, Box::new(|st, _app, _, _| views::go(st, Page::Albums)));
    step(1500, Box::new(|_st, app, r, _| {
        // Tab: the filter field first, then Sort, the direction button, the grid.
        tabs(app, 1);
        type_text(app, "mn s");
        let g = app.global::<Grid>();
        line(r, format!(
            "typed \"mn s\" into the filter after one Tab: filter {:?}, muted {}, shuffle {}, page {:?}",
            g.get_filter().to_string(),
            app.global::<crate::Player>().get_muted(),
            app.global::<crate::Player>().get_shuffle(),
            app.global::<Nav>().get_page()
        ));
    }));
    step(800, Box::new(|st, app, _, _| {
        snapshot(app, "p4-23-key-filter-typing");
        app.global::<Grid>().set_filter("".into());
        views::filter_grid(st, "");
        tabs(app, 1);
    }));
    step(400, Box::new(|_st, app, _, _| {
        snapshot(app, "p4-24-key-sort-button");
        tabs(app, 2);
    }));
    step(400, Box::new(|_st, app, _, _| {
        snapshot(app, "p4-25-key-grid");
        press(app, Key::RightArrow, &[]);
        press(app, Key::RightArrow, &[]);
        press(app, Key::DownArrow, &[]);
    }));
    step(500, Box::new(|st, app, r, _| {
        snapshot(app, "p4-26-key-grid-moved");
        let cols = grid_cols(app);
        let expect = st.models.grid.row_data(2 + cols).map(|a| (a.id, a.title.to_string()));
        line(r, format!("grid: Right Right Down with {cols} columns: expecting album {expect:?}"));
        press(app, Key::Return, &[]);
        let open = app.global::<crate::AlbumPage>().get_album();
        line(r, format!(
            "  Enter opened: page {:?}, album {} {:?} -> {}",
            app.global::<Nav>().get_page(),
            open.id,
            open.title.to_string(),
            if expect.map(|e| e.0) == Some(open.id) { "PASS" } else { count_fail(); "FAIL" }
        ));
    }));
    step(1000, Box::new(|st, app, _, _| {
        snapshot(app, "p4-27-key-album");
        views::back(st);
    }));
    step(1200, Box::new(|_st, app, _, _| {
        // Deep into the virtualised grid: the rows on screen come and go, the focus stays.
        // Page Down as a held key repeats it, a frame between presses.
        tabs(app, 4);
    }));
    for _ in 0..12 {
        step(60, Box::new(|_st, app, _, _| press(app, Key::PageDown, &[])));
    }
    step(60, Box::new(|_st, app, _, _| press(app, Key::LeftArrow, &[])));
    step(800, Box::new(|st, app, r, _| {
        snapshot(app, "p4-28-key-grid-deep");
        let g = app.global::<Grid>();
        line(r, format!("grid: Tab x4, 12 x Page Down 60 ms apart, Left: scrolled to {:.0} px (p4-28 shows where the cursor is)", g.get_content_y()));
        let _ = st;
        press(app, Key::Home, &[]);
    }));
    step(800, Box::new(|st, app, r, _| {
        line(r, format!("  Home: scrolled back to {:.0} px", app.global::<Grid>().get_content_y()));
        press(app, Key::End, &[]);
        press(app, Key::LeftArrow, &[]);
        let n = app.global::<Grid>().get_count() as usize;
        let expect = st.models.grid.row_data(n - 2).map(|a| a.id);
        press(app, Key::Return, &[]);
        let open = app.global::<crate::AlbumPage>().get_album();
        line(r, format!(
            "  End, Left, Enter, the rows recycled twice: album {} {:?} -> {}",
            open.id,
            open.title.to_string(),
            if expect == Some(open.id) { "PASS" } else { count_fail(); "FAIL" }
        ));
        if let Some(a) = album_id(st, "Lemonade") {
            views::open_album(st, a);
        }
    }));
    step(1200, Box::new(|_st, app, _, _| {
        // Album page: the artist link, Play, Shuffle, Play next, Add to queue, the heart,
        // the album's menu button, then the track list.
        tabs(app, 8);
        press(app, Key::DownArrow, &[]);
    }));
    step(500, Box::new(|st, app, r, _| {
        snapshot(app, "p4-29-key-tracklist");
        press(app, Key::Return, &[]);
        let playing = st.queue.borrow().current().map(|i| i.track.title.clone());
        line(r, format!("album page: Tab x8, Down, Enter: now playing {playing:?} (expect \"Track 2 of Lemonade\")"));
        verdict(&mut r.borrow_mut(), "Enter on a track row plays it", playing.as_deref() == Some("Hold Up"), format!("{playing:?}"));
        playback::set_paused(st, true);
    }));
    step(600, Box::new(|_st, app, r, _| {
        press(app, Key::Menu, &[]);
        let m = app.global::<crate::MenuStore>();
        line(r, format!("Menu key: menu open {}, from keys {}, highlight {}", m.get_open(), m.get_from_keys(), m.get_highlight()));
    }));
    step(400, Box::new(|st, app, r, _| {
        snapshot(app, "p4-30-key-menu");
        let m = app.global::<crate::MenuStore>();
        let before = st.queue.borrow().items().len();
        press(app, Key::DownArrow, &[]);
        press(app, Key::DownArrow, &[]);
        line(r, format!("  Down Down: highlight {} ({:?})", m.get_highlight(), m.get_items().row_data(m.get_highlight().max(0) as usize).map(|i| i.label.to_string())));
        press(app, Key::Return, &[]);
        line(r, format!("  Enter: menu open {}, queue {} -> {} tracks", m.get_open(), before, st.queue.borrow().items().len()));
        verdict(&mut r.borrow_mut(), "the Menu key's Add to queue adds the track", !m.get_open() && st.queue.borrow().items().len() == before + 1, "");
        press(app, Key::F10, &[Key::Shift]);
        line(r, format!("  Shift+F10: menu open {}", m.get_open()));
        press(app, Key::Escape, &[]);
        line(r, format!("  Esc: menu open {}, page {:?} (the menu closes, the page stays)", m.get_open(), app.global::<Nav>().get_page()));
        press(app, Key::DownArrow, &[]);
    }));
    step(500, Box::new(|st, app, r, _| {
        snapshot(app, "p4-31-key-tracklist-after-menu");
        // Space on the list: the shortcut still pauses and plays (the engine's paused flag,
        // which the player bar follows on its next tick).
        let before = st.player.is_paused();
        press(app, Key::Space, &[]);
        let after = st.player.is_paused();
        press(app, Key::Space, &[]);
        let again = st.player.is_paused();
        line(r, format!("Space with the focus on the track list: paused {before} -> {after} -> {again}"));
        playback::set_paused(st, true);
        // Plain-letter and Ctrl shortcuts, paused (the volume goes back to 0 after).
        let p = app.global::<crate::Player>();
        let (m0, s0, r0) = (p.get_muted(), p.get_shuffle(), p.get_repeat());
        type_text(app, "m");
        let m1 = p.get_muted();
        type_text(app, "m");
        type_text(app, "s");
        let s1 = p.get_shuffle();
        type_text(app, "s");
        type_text(app, "r");
        let r1 = p.get_repeat();
        type_text(app, "rr");
        let f0 = p.get_favourite();
        type_text(app, "l");
        let f1 = p.get_favourite();
        type_text(app, "l");
        let v0 = st.settings.borrow().volume;
        press(app, Key::UpArrow, &[Key::Control]);
        let v1 = st.settings.borrow().volume;
        press(app, Key::DownArrow, &[Key::Control]);
        let v2 = st.settings.borrow().volume;
        playback::set_volume(st, 0.0);
        line(r, format!("M: muted {m0} -> {m1}; S: shuffle {s0} -> {s1}; R: repeat {r0} -> {r1}; L: favourite {f0} -> {f1}; Ctrl+Up, Ctrl+Down: volume {v0} -> {v1} -> {v2}"));
        type_text(app, "n");
        line(r, format!("N: page {:?}", app.global::<Nav>().get_page()));
    }));
    step(1000, Box::new(|_st, app, r, _| {
        snapshot(app, "p4-32-key-now-playing");
        type_text(app, "n");
        line(r, format!("N again: page {:?}", app.global::<Nav>().get_page()));
    }));
    step(800, Box::new(|_st, app, r, _| {
        // "/" goes to search with the field focused; letters then type, not act.
        type_text(app, "/");
        type_text(app, "mns");
        let p = app.global::<crate::Player>();
        line(r, format!(
            "/ then \"mns\": page {:?}, query {:?}, muted {}, shuffle {}",
            app.global::<Nav>().get_page(),
            app.global::<Search>().get_query().to_string(),
            p.get_muted(),
            p.get_shuffle()
        ));
        press(app, Key::LeftArrow, &[Key::Alt]);
        line(r, format!("  Alt+Left in the field: page {:?} (expect Album: back, as anywhere)", app.global::<Nav>().get_page()));
        press(app, Key::RightArrow, &[Key::Alt]);
        app.invoke_focus_search();
        type_text(app, " ");
        line(r, format!(
            "  Alt+Right (expect Search), then Space in the field: page {:?}, query {:?}, paused {}",
            app.global::<Nav>().get_page(),
            app.global::<Search>().get_query().to_string(),
            st_paused(app)
        ));
        press(app, Key::Escape, &[]);
        line(r, format!("  Esc in the field: page {:?}", app.global::<Nav>().get_page()));
    }));
    step(800, Box::new(|st, app, _, pl| {
        // The playlist page: Play, Rename, Delete, then the list.
        views::open_playlist(st, pl.get());
        app.invoke_focus_shortcuts();
    }));
    step(1000, Box::new(|st, app, r, pl| {
        // Play, Shuffle, Rename, Export..., Delete, then the list.
        tabs(app, 6);
        press(app, Key::DownArrow, &[]);
        let before = playlist_titles(st, pl.get());
        press(app, Key::DownArrow, &[Key::Alt]);
        line(r, format!("playlist: Tab x6, Down, Alt+Down: {before:?} -> {:?}", playlist_titles(st, pl.get())));
        verdict(&mut r.borrow_mut(), "Alt+Down moves the focused playlist row", playlist_titles(st, pl.get()) != before, "");
    }));
    step(600, Box::new(|st, app, r, pl| {
        snapshot(app, "p4-33-key-playlist-moved");
        press(app, Key::Delete, &[]);
        line(r, format!("  Delete: {:?}", playlist_titles(st, pl.get())));
        verdict(&mut r.borrow_mut(), "Delete removes the focused playlist row", playlist_titles(st, pl.get()).len() == 5, playlist_titles(st, pl.get()).len());
        press(app, Key::Tab, &[Key::Shift]);
    }));
    step(500, Box::new(|st, app, _, _| {
        snapshot(app, "p4-34-key-shift-tab");
        views::go(st, Page::NowPlaying);
    }));
    step(1000, Box::new(|st, app, r, _| {
        // Now playing: the artist link, the album link, shuffle, then the queue.
        // The artist and album links, Up next and Lyrics, shuffle, then the queue.
        tabs(app, 6);
        let before = queue_titles(st);
        press(app, Key::DownArrow, &[]);
        press(app, Key::Delete, &[]);
        line(r, format!("queue on Now playing: Tab x6, Down, Delete: {} -> {} tracks {:?}", before.len(), queue_titles(st).len(), queue_titles(st)));
        verdict(&mut r.borrow_mut(), "Delete removes the focused queue row", queue_titles(st).len() + 1 == before.len(), queue_titles(st).len());
    }));
    step(500, Box::new(|st, app, _, _| {
        snapshot(app, "p4-35-key-queue");
        views::go(st, Page::Home);
    }));
    step(1200, Box::new(|st, app, r, _| {
        snapshot(app, "p4-36-home");
        // Where the volume slider is: Tab on until Up moves the volume. Paused first, so
        // nothing is heard while the volume is above 0.
        playback::set_paused(st, true);
        app.invoke_focus_shortcuts();
        let mut found = None;
        for i in 1..120 {
            press(app, Key::Tab, &[]);
            if app.global::<crate::Focus>().get_zone() != "bar" {
                continue;
            }
            let v0 = st.settings.borrow().volume;
            press(app, Key::UpArrow, &[]);
            if (st.settings.borrow().volume - v0).abs() > 0.001 {
                found = Some(i);
                break;
            }
        }
        line(r, format!("volume slider reached after {found:?} Tabs from the top of Home; Up: volume {}", st.settings.borrow().volume));
    }));
    step(400, Box::new(|st, app, r, _| {
        snapshot(app, "p4-37-key-volume-slider");
        press(app, Key::DownArrow, &[]);
        line(r, format!("  Down: volume {}", st.settings.borrow().volume));
        playback::set_volume(st, 0.0);
        app.global::<Nav>().set_sidebar_forced(true);
    }));
    step(600, Box::new(|_st, app, r, _| {
        // Into the sidebar with the keyboard: it stays out while it has the focus.
        let mut n = 0;
        for i in 1..60 {
            press(app, Key::Tab, &[]);
            if app.global::<crate::Focus>().get_zone() == "sidebar" {
                n = i;
                break;
            }
        }
        app.global::<Nav>().set_sidebar_forced(false);
        line(r, format!("sidebar: reached after {n} more Tabs; focus zone {:?}", app.global::<crate::Focus>().get_zone().to_string()));
    }));
    step(800, Box::new(|_st, app, r, _| {
        snapshot(app, "p4-38-key-sidebar");
        line(r, format!("  sidebar still out with the forced open cleared (the focus holds it): {}", app.global::<crate::Focus>().get_zone() == "sidebar"));
        press(app, Key::Escape, &[]);
        line(r, format!("  Esc: focus zone {:?}, sidebar forced {}", app.global::<crate::Focus>().get_zone().to_string(), app.global::<Nav>().get_sidebar_forced()));
    }));
    step(800, Box::new(|st, app, r, pl| {
        snapshot(app, "p4-39-after-esc");
        let _ = library::query::delete_playlist(&st.db, pl.get());
        write_report("phase4", &r.borrow());
        let _ = slint::quit_event_loop();
    }));
    drop(step);
    run_steps(steps);
}
