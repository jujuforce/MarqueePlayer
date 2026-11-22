//! `MP_BENCH=autoscroll`: middle-click scrolling (`autoscroll.rs`), driven by pointer and
//! key events dispatched in the app (no OS input; the real pointer never moves). Each event
//! goes through `autoscroll::handle`, the same filter winit's events go through, and what
//! it lets through is dispatched to Slint (`Window::dispatch_event`), as the window would.
//! On a page (Home), a horizontal shelf (Home), the virtualised album grid, the queue (now
//! playing, and the queue sidebar over the grid), Settings and the lyrics: that each
//! scrolls at the curve's speed, stops at its end (and stops stepping there), and that
//! the mode ends on a left, right and middle click (none of which reaches the page), the
//! wheel, Esc, a page change, the window losing focus and a held drag's release; that a
//! middle click on a cover opens nothing, one on what does not scroll does nothing (and
//! the next click is a click), and that with the setting off the middle button passes.
//! Reported in `bench-autoscroll.txt` with snapshots (`as-*.png`).
//!
//! `MP_BENCH=tray`: the animated tray icon (`tray/anim.rs`), the window hidden. Playing
//! with the animation off, then on (`MP_BENCH_TRAY_SECS` each, default 30): process CPU,
//! frames and icons sent, the time a frame takes, the process's GDI and USER objects every
//! 10 s; then a pause: the settle frames, and that the timer stops; then idle,
//! paused and hidden, for `MP_BENCH_IDLE_SECS` (default 60). Contact sheets of the frames
//! at 16 and 32 px on dark and light grounds (`tray-sheet-*.png`). Report:
//! `bench-tray.txt`, with each phase's start time (unix) for an outside count of context
//! switches. Then the icon's gestures (`tray_gestures`): click, double click, middle
//! button and wheel, handed to the icon's window inside the app (no real input). Needs
//! `MP_MUSIC` with the "Groove" fixture album.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

use crate::autoscroll::{self, Button, Input, Mode};
use crate::bench::{Step, out_dir, run_steps, snapshot, write_report};
use crate::state::State;
use crate::{AppWindow, Autoscroll, Grid, Lyrics, MenuStore, Nav, Page, playback, views};

type Report = Rc<RefCell<String>>;

fn line(r: &Report, text: String) {
    log::info!("autoscroll bench: {text}");
    r.borrow_mut().push_str(&text);
    r.borrow_mut().push('\n');
}

fn check(r: &Report, fails: &Rc<Cell<u32>>, what: &str, ok: bool, detail: String) {
    if !ok {
        fails.set(fails.get() + 1);
        crate::bench::count_fail();
    }
    line(r, format!("  [{}] {what}: {detail}", if ok { "ok" } else { "FAIL" }));
}

/// One input, as the window would get it: through the autoscroll filter, then (unless it
/// kept it) to Slint.
fn send(app: &AppWindow, ev: Input) -> bool {
    let kept = autoscroll::handle(app, ev);
    if kept {
        return true;
    }
    let w = app.window();
    let at = |x: f32, y: f32| LogicalPosition::new(x, y);
    let button = |b: Button| match b {
        Button::Left => PointerEventButton::Left,
        Button::Right => PointerEventButton::Right,
        Button::Middle => PointerEventButton::Middle,
        Button::Other => PointerEventButton::Other,
    };
    match ev {
        Input::Move(x, y) => w.dispatch_event(WindowEvent::PointerMoved { position: at(x, y) }),
        Input::Press(b, x, y) => w.dispatch_event(WindowEvent::PointerPressed { position: at(x, y), button: button(b) }),
        Input::Release(b, x, y) => w.dispatch_event(WindowEvent::PointerReleased { position: at(x, y), button: button(b) }),
        Input::Wheel => {
            let p = POINTER.with(Cell::get);
            w.dispatch_event(WindowEvent::PointerScrolled { position: at(p.0, p.1), delta_x: 0.0, delta_y: -120.0 })
        }
        Input::Escape => w.dispatch_event(WindowEvent::KeyPressed { text: Key::Escape.into() }),
        Input::EscapeUp => w.dispatch_event(WindowEvent::KeyReleased { text: Key::Escape.into() }),
        Input::FocusLost | Input::Away => {}
    }
    false
}

thread_local! {
    static POINTER: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
    static CY0: Cell<u64> = const { Cell::new(0) };
    static HZ: Cell<f64> = const { Cell::new(3.0e9) };
}

pub(super) fn move_to(app: &AppWindow, x: f32, y: f32) {
    POINTER.with(|p| p.set((x, y)));
    send(app, Input::Move(x, y));
}

/// Press and release (a click) of `b` at the pointer; returns whether each was kept.
pub(super) fn click(app: &AppWindow, b: Button) -> (bool, bool) {
    let (x, y) = POINTER.with(Cell::get);
    (send(app, Input::Press(b, x, y)), send(app, Input::Release(b, x, y)))
}

fn size(app: &AppWindow) -> (f32, f32) {
    let s = app.window().size().to_logical(app.window().scale_factor());
    (s.width, s.height)
}

fn state(app: &AppWindow) -> String {
    let g = app.global::<Autoscroll>();
    format!(
        "mode {:?}, engaged {}, axes {}{}, at ({:.1}, {:.1}) of ({:.1}, {:.1}), v ({:.0}, {:.0}), frames {}, page {:?}, page-scroll {:.1}, grid {:.1}",
        autoscroll::mode(),
        g.get_engaged(),
        if g.get_has_x() { "x" } else { "" },
        if g.get_has_y() { "y" } else { "" },
        g.get_at_x(),
        g.get_at_y(),
        g.get_end_x(),
        g.get_end_y(),
        g.get_vx(),
        g.get_vy(),
        g.get_frames(),
        app.global::<Nav>().get_page(),
        app.global::<Nav>().get_page_scroll(),
        app.global::<Grid>().get_content_y(),
    )
}

fn album_tracks(st: &Rc<State>, title: &str) -> Vec<library::Track> {
    let id: Option<i64> = st.db.query_row("SELECT id FROM albums WHERE title = ?1", [title], |r| r.get(0)).ok();
    id.map(|a| library::query::album_tracks(&st.db, a).unwrap_or_default()).unwrap_or_default()
}

/// What a probe found, for the steps after it.
type Found = Rc<Cell<Option<(f32, f32)>>>;

/// Middle-click each point in turn (ending each try at once) until one engages with the
/// axes wanted; the point lands in `found`. Runs on its own timers, 60 ms a point.
fn probe(points: Vec<(f32, f32)>, want_x: bool, want_y: bool, found: Found) {
    fn go(mut points: std::vec::IntoIter<(f32, f32)>, want: (bool, bool), found: Found) {
        let Some((x, y)) = points.next() else { return };
        crate::state::with(|st| {
            if let Some(app) = st.app.upgrade() {
                move_to(&app, x, y);
                click(&app, Button::Middle);
            }
        });
        slint::Timer::single_shot(Duration::from_millis(60), move || {
            let hit = crate::state::with(|st| {
                let app = st.app.upgrade()?;
                let g = app.global::<Autoscroll>();
                let ok = g.get_engaged() && g.get_has_x() == want.0 && g.get_has_y() == want.1;
                autoscroll::away(&app);
                Some(ok)
            })
            .flatten()
            .unwrap_or(false);
            if hit {
                found.set(Some((x, y)));
            } else {
                go(points, want, found);
            }
        });
    }
    found.set(None);
    go(points.into_iter(), (want_x, want_y), found);
}

pub fn run() {
    let r: Report = Rc::default();
    let fails: Rc<Cell<u32>> = Rc::default();
    let mut steps: Vec<Step> = Vec::new();
    macro_rules! step {
        ($ms:expr, $f:expr) => {{
            let r = r.clone();
            let fails = fails.clone();
            let f: Box<dyn FnOnce(&Rc<State>, &AppWindow, &Report, &Rc<Cell<u32>>)> = Box::new($f);
            steps.push(($ms, Box::new(move |st: &Rc<State>, app: &AppWindow| f(st, app, &r, &fails))));
        }};
    }
    let found: Found = Rc::default();
    let t0: Rc<Cell<(Instant, f32, i32)>> = Rc::new(Cell::new((Instant::now(), 0.0, 0)));
    let cfg = autoscroll::config();

    step!(600, |st, app, r, _| {
        let (w, h) = size(app);
        line(r, format!("window {w}x{h} logical, scale {}; curve {:?}", app.window().scale_factor(), autoscroll::config()));
        line(r, format!("library: {:?}", library::query::counts(&st.db)));
        playback::play_tracks(st, album_tracks(st, "Long Album"), 0);
        views::go(st, Page::Home);
    });
    step!(1500, |st, _app, _r, _| playback::set_paused(st, true));

    // ---- Home: the page (vertical) --------------------------------------------------
    {
        let found = found.clone();
        step!(400, move |_st, app, r, _| {
            line(r, "[home] looking for a point where only the page scrolls, and one on a shelf".into());
            let (w, h) = size(app);
            let pts = (0..30).map(|i| (w * 0.5, 120.0 + i as f32 * 24.0)).filter(|p| p.1 < h - 100.0).collect();
            probe(pts, false, true, found);
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(2200, move |_st, app, r, fails| {
            let Some((x, y)) = found.get() else {
                check(r, fails, "found a page point", false, state(app));
                return;
            };
            line(r, format!("[home] page point at {x},{y}"));
            move_to(app, x, y);
            let kept = click(app, Button::Middle);
            check(r, fails, "middle click kept from the page", kept == (true, true), format!("{kept:?}"));
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "on, toggled, page claims y only", autoscroll::mode() == Mode::Toggled && g.get_has_y() && !g.get_has_x(), state(app));
            snapshot(app, "as-01-home-anchor");
            let (x, y) = found.get().unwrap_or_default();
            // Far below: fast, to the end.
            move_to(app, x, y + 400.0);
            t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
        });
    }
    {
        let t0 = t0.clone();
        step!(1500, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "reached the end", (g.get_at_y() - g.get_end_y()).abs() < 0.01, state(app));
            check(r, fails, "page-scroll follows (scroll restore sees it)", (app.global::<Nav>().get_page_scroll() - g.get_at_y()).abs() < 0.01, state(app));
            t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(500, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "at the end, nothing steps (no frames)", g.get_frames() == t0.get().2, state(app));
            snapshot(app, "as-03-home-end");
            // Above the anchor: back up.
            let (x, y) = found.get().unwrap_or_default();
            move_to(app, x, y - 60.0);
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(500, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "scrolls back up above the anchor", g.get_at_y() > g.get_end_y() + 5.0, state(app));
            // Into the dead zone (10 px off the anchor on each axis).
            let (x, y) = found.get().unwrap_or_default();
            move_to(app, x + 10.0, y - 10.0);
            t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
        });
    }
    {
        let t0 = t0.clone();
        step!(100, move |_st, app, _r, _| {
            let g = app.global::<Autoscroll>();
            t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
        });
    }
    {
        let t0 = t0.clone();
        step!(600, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "in the dead zone: still, no frames", g.get_frames() == t0.get().2 && g.get_at_y() == t0.get().1, state(app));
            // A left click ends it, and is not a click on the page.
            let page = app.global::<Nav>().get_page();
            let kept = click(app, Button::Left);
            check(r, fails, "left click ends it, kept from the page", kept == (true, true) && autoscroll::mode() == Mode::Off && !g.get_on(), format!("{kept:?}, {}", state(app)));
            check(r, fails, "still on the same page", app.global::<Nav>().get_page() == page, format!("{page:?}"));
        });
    }

    // ---- Home: a shelf (horizontal) inside the page --------------------------------
    step!(300, |_st, app, _r, _| {
        app.global::<Nav>().set_page_scroll(0.0);
    });
    {
        let found = found.clone();
        step!(300, move |_st, app, r, _| {
            line(r, "[shelf] looking for a shelf row".into());
            let (w, h) = size(app);
            let pts = (0..40).map(|i| (w * 0.5, 100.0 + i as f32 * 20.0)).filter(|p| p.1 < h - 100.0).collect();
            probe(pts, true, true, found);
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(2600, move |_st, app, r, fails| {
            let Some((x, y)) = found.get() else {
                check(r, fails, "found a shelf", false, state(app));
                return;
            };
            line(r, format!("[shelf] shelf at {x},{y}: the shelf takes x, the page around it y (as in browsers)"));
            move_to(app, x, y);
            click(app, Button::Middle);
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    {
        let t0 = t0.clone();
        step!(200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "both axes, the cursor is the four-way one", g.get_has_x() && g.get_has_y(), state(app));
            let (x, y) = POINTER.with(Cell::get);
            move_to(app, x + 160.0, y);
            t0.set((Instant::now(), g.get_at_x(), g.get_frames()));
        });
    }
    {
        let t0 = t0.clone();
        step!(800, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            let (t, x0, _) = t0.get();
            let moved = x0 - g.get_at_x();
            let want = autoscroll::speed(&cfg, 160.0);
            line(r, format!("[shelf] 160 px right: moved {moved:.1} px in {:.2} s (curve {want:.0} px/s)", t.elapsed().as_secs_f32()));
            check(r, fails, "the shelf scrolls right; the page stays", moved > 50.0 || (g.get_at_x() - g.get_end_x()).abs() < 0.01, state(app));
            check(r, fails, "page unmoved by a horizontal pointer", app.global::<Nav>().get_page_scroll() == 0.0, state(app));
            snapshot(app, "as-04-shelf-scrolling");
        });
    }
    step!(2500, |_st, app, r, fails| {
        let g = app.global::<Autoscroll>();
        check(r, fails, "the shelf stops at its end", (g.get_at_x() - g.get_end_x()).abs() < 0.01, state(app));
        // A right click ends it; no menu opens.
        let kept = click(app, Button::Right);
        check(r, fails, "right click ends it, no menu", kept == (true, true) && autoscroll::mode() == Mode::Off && !app.global::<MenuStore>().get_open(), state(app));
        snapshot(app, "as-05-shelf-end");
    });

    // ---- The album grid (virtualised) ------------------------------------------------
    step!(300, |st, _app, _r, _| views::go(st, Page::Albums));
    {
        let t0 = t0.clone();
        step!(1500, move |_st, app, r, fails| {
            let (w, _h) = size(app);
            // On a cover: a middle click opens nothing.
            move_to(app, w * 0.5, 420.0);
            let kept = click(app, Button::Middle);
            check(r, fails, "middle click on a cover kept", kept == (true, true), format!("{kept:?}"));
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    {
        let t0 = t0.clone();
        step!(200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "on the grid, no album opened", g.get_engaged() && g.get_has_y() && app.global::<Nav>().get_page() == Page::Albums, state(app));
            let (x, y) = POINTER.with(Cell::get);
            move_to(app, x, y + 60.0);
            t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
        });
    }
    // Speed: pixels per second as the curve says, whatever the frame rate.
    for (k, offset, ms) in [(0, 60.0f32, 2000u64), (1, 160.0, 1000)] {
        let t0 = t0.clone();
        step!(ms, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            let (t, y0, f0) = t0.get();
            let secs = t.elapsed().as_secs_f32();
            let moved = y0 - g.get_at_y();
            let want = autoscroll::speed(&cfg, offset);
            let frames = g.get_frames() - f0;
            line(r, format!("[grid] {offset} px below: {moved:.1} px in {secs:.2} s = {:.0} px/s (curve {want:.0}); {frames} frames, {:.1} per s", moved / secs, frames as f32 / secs));
            check(r, fails, "speed matches the curve", (moved / secs - want).abs() < want * 0.1 + 3.0, format!("{:.0} vs {want:.0} px/s", moved / secs));
            let (x, y) = POINTER.with(Cell::get);
            if k == 0 {
                snapshot(app, "as-06-grid-scrolling");
                move_to(app, x, y + 100.0);
                t0.set((Instant::now(), g.get_at_y(), g.get_frames()));
            } else {
                // To the end, fast.
                move_to(app, x, y + 260.0);
            }
        });
    }
    {
        let t0 = t0.clone();
        step!(2500, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "the grid reached its end; Grid.content-y follows", (g.get_at_y() - g.get_end_y()).abs() < 0.01 && (app.global::<Grid>().get_content_y() - g.get_at_y()).abs() < 0.01, state(app));
            snapshot(app, "as-06b-grid-end");
            // The wheel ends it and does not scroll.
            let before = g.get_at_y();
            let kept = send(app, Input::Wheel);
            check(r, fails, "the wheel ends it", kept && autoscroll::mode() == Mode::Off, state(app));
            t0.set((Instant::now(), before, 0));
        });
    }
    {
        let t0 = t0.clone();
        step!(400, move |_st, app, r, fails| {
            let y = app.global::<Grid>().get_content_y();
            check(r, fails, "the wheel that ended it did not scroll", (y - t0.get().1).abs() < 0.5, format!("{y:.1} vs {:.1}", t0.get().1));
            snapshot(app, "as-07-grid-after");
            // The next click is a click: on a cover (the first, back at the top), it opens
            // the album.
            app.global::<Grid>().set_content_y(0.0);
        });
    }
    {
        step!(300, move |_st, app, r, fails| {
            move_to(app, 131.0, 220.0);
            let kept = click(app, Button::Left);
            check(r, fails, "the next click goes through", kept == (false, false), format!("{kept:?}"));
        });
    }
    step!(800, |st, app, r, fails| {
        check(r, fails, "and opened the album", app.global::<Nav>().get_page() == Page::Album, state(app));
        views::back(st);
    });
    step!(800, |_st, app, _r, _| {
        // The queue sidebar slid in over the grid: the queue wins (it is on top).
        app.global::<Nav>().set_queue_open(true);
    });
    {
        let t0 = t0.clone();
        step!(600, move |_st, app, r, fails| {
            let (w, _) = size(app);
            move_to(app, w - 170.0, 420.0);
            click(app, Button::Middle);
            t0.set((Instant::now(), app.global::<Grid>().get_content_y(), 0));
            let _ = (r, fails);
        });
    }
    {
        let t0 = t0.clone();
        step!(200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "queue sidebar claims, not the grid under it", g.get_has_y() && g.get_end_y() > -3000.0, state(app));
            let (x, y) = POINTER.with(Cell::get);
            // Down and well to the left, over the grid: the queue keeps scrolling.
            move_to(app, x - 500.0, y + 120.0);
            t0.set((Instant::now(), app.global::<Grid>().get_content_y(), g.get_frames()));
        });
    }
    {
        let t0 = t0.clone();
        step!(1200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "the queue scrolls; the grid under it does not", g.get_at_y() < -10.0 && app.global::<Grid>().get_content_y() == t0.get().1, state(app));
            check(r, fails, "the sidebar stays open while it scrolls, pointer away", app.global::<Nav>().get_queue_hover_open() || app.global::<Nav>().get_queue_open(), state(app));
            snapshot(app, "as-08-queue-sidebar");
            // Middle click again ends it.
            let kept = click(app, Button::Middle);
            check(r, fails, "a middle click ends it", kept == (true, true) && autoscroll::mode() == Mode::Off, state(app));
            app.global::<Nav>().set_queue_open(false);
            app.global::<Nav>().set_queue_hover_open(false);
        });
    }

    // ---- Now playing: Up next -------------------------------------------------------
    step!(500, |st, app, _r, _| {
        app.global::<Lyrics>().set_shown(false);
        views::go(st, Page::NowPlaying);
    });
    {
        let t0 = t0.clone();
        step!(1200, move |_st, app, _r, _| {
            let (w, _) = size(app);
            move_to(app, w - 250.0, 400.0);
            click(app, Button::Middle);
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    step!(200, |_st, app, r, fails| {
        let g = app.global::<Autoscroll>();
        check(r, fails, "Up next claims", g.get_engaged() && g.get_has_y(), state(app));
        let (x, y) = POINTER.with(Cell::get);
        move_to(app, x, y + 150.0);
    });
    step!(1000, |st, app, r, fails| {
        let g = app.global::<Autoscroll>();
        check(r, fails, "Up next scrolls", g.get_at_y() < -10.0, state(app));
        snapshot(app, "as-09-up-next");
        // A page change ends it.
        views::go(st, Page::Home);
    });
    step!(300, |st, app, r, fails| {
        check(r, fails, "a page change ends it", autoscroll::mode() == Mode::Off && !app.global::<Autoscroll>().get_on(), state(app));
        views::back(st);
    });

    // ---- Settings: the column; the held form -----------------------------------------
    step!(600, |st, _app, _r, _| views::settings_section(st, crate::settings::nav::KEYS));
    {
        let found = found.clone();
        step!(1200, move |_st, app, r, _| {
            line(r, "[settings] looking for the column".into());
            let (w, _h) = size(app);
            let pts = (0..16).map(|i| (120.0 + i as f32 * (w - 240.0) / 16.0, 500.0)).collect();
            probe(pts, false, true, found);
        });
    }
    {
        let (found, t0) = (found.clone(), t0.clone());
        step!(1300, move |_st, app, r, fails| {
            let Some((x, y)) = found.get() else {
                check(r, fails, "found the column", false, state(app));
                return;
            };
            line(r, format!("[settings] column at {x},{y}; held: press, drag down, release"));
            move_to(app, x, y);
            let kept = send(app, Input::Press(Button::Middle, x, y));
            check(r, fails, "held press kept", kept, state(app));
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    step!(150, |_st, app, _r, _| {
        let (x, y) = POINTER.with(Cell::get);
        move_to(app, x, y + 140.0);
    });
    step!(900, |_st, app, r, fails| {
        let g = app.global::<Autoscroll>();
        check(r, fails, "held: scrolling while the button is down", autoscroll::mode() == Mode::Held { moved: true } && g.get_at_y() < -10.0, state(app));
        snapshot(app, "as-10-settings-held");
        let (x, y) = POINTER.with(Cell::get);
        let kept = send(app, Input::Release(Button::Middle, x, y));
        check(r, fails, "release after moving ends it", kept && autoscroll::mode() == Mode::Off, state(app));
        check(r, fails, "Nav.page-scroll has the column's scroll (Back restores it)", (app.global::<Nav>().get_page_scroll() - g.get_at_y()).abs() < 0.01, state(app));
    });
    {
        let found = found.clone();
        step!(300, move |_st, app, r, fails| {
            // A quick click without moving leaves it on.
            let (x, y) = found.get().unwrap_or_default();
            move_to(app, x, y);
            send(app, Input::Press(Button::Middle, x, y));
            move_to(app, x + 3.0, y + 2.0);
            send(app, Input::Release(Button::Middle, x + 3.0, y + 2.0));
            check(r, fails, "a quick click (3 px) leaves it on", autoscroll::mode() == Mode::Toggled, state(app));
        });
    }
    step!(200, |st, app, r, fails| {
        // Esc ends it, and does not go back.
        let page = app.global::<Nav>().get_page();
        let kept = (send(app, Input::Escape), send(app, Input::EscapeUp));
        check(r, fails, "Esc ends it, kept", kept == (true, true) && autoscroll::mode() == Mode::Off, state(app));
        check(r, fails, "Esc did not go back", app.global::<Nav>().get_page() == page && st.history.borrow().current == crate::nav::Route::Settings(crate::settings::nav::KEYS), format!("{page:?}"));
        // The section list does not scroll: a middle click there does nothing, and the
        // next click on a section goes through.
        app.global::<Nav>().set_page_scroll(0.0);
    });
    {
        let t0 = t0.clone();
        step!(300, move |_st, app, _r, _| {
            move_to(app, 100.0, 200.0);
            click(app, Button::Middle);
            t0.set((Instant::now(), 0.0, app.global::<crate::SettingsView>().get_section()));
        });
    }
    step!(200, |_st, app, r, fails| {
        let g = app.global::<Autoscroll>();
        check(r, fails, "middle click on the section list (does not scroll): nothing", !g.get_engaged(), state(app));
        snapshot(app, "as-11-settings-nothing");
    });

    // ---- Lyrics -------------------------------------------------------------------
    step!(300, |st, app, _r, _| {
        playback::play_tracks(st, album_tracks(st, "Groove"), 0);
        app.global::<Lyrics>().set_shown(true);
        views::go(st, Page::NowPlaying);
    });
    {
        let t0 = t0.clone();
        step!(2500, move |_st, app, r, _| {
            let l = app.global::<Lyrics>();
            line(r, format!("[lyrics] {} lines, synced {}, current {}", l.get_lines().row_count(), l.get_synced(), l.get_current()));
            let (w, _) = size(app);
            move_to(app, w - 250.0, 300.0);
            click(app, Button::Middle);
            t0.set((Instant::now(), 0.0, 0));
        });
    }
    {
        let t0 = t0.clone();
        step!(200, move |_st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "the lyrics claim", g.get_engaged() && g.get_has_y(), state(app));
            let (x, y) = POINTER.with(Cell::get);
            move_to(app, x, y + 90.0);
            t0.set((Instant::now(), g.get_at_y(), 0));
        });
    }
    {
        let t0 = t0.clone();
        step!(2500, move |st, app, r, fails| {
            let g = app.global::<Autoscroll>();
            check(r, fails, "the lyrics scroll down past the sung line (following holds off)", g.get_at_y() < t0.get().1 - 100.0, state(app));
            snapshot(app, "as-12-lyrics");
            // The window losing focus ends it (winit's Focused(false) becomes this).
            autoscroll::handle(app, Input::FocusLost);
            check(r, fails, "focus loss ends it", autoscroll::mode() == Mode::Off && !g.get_on(), state(app));
            playback::set_paused(st, true);
        });
    }

    // ---- The setting off -------------------------------------------------------------
    step!(300, |st, app, _r, _| {
        st.settings.borrow_mut().autoscroll = false;
        views::refresh_player_settings(st);
        views::go(st, Page::Albums);
        let _ = app;
    });
    step!(1200, |_st, app, r, fails| {
        let (w, _) = size(app);
        move_to(app, w * 0.5, 420.0);
        let kept = click(app, Button::Middle);
        check(r, fails, "setting off: the middle button passes", kept == (false, false) && autoscroll::mode() == Mode::Off, state(app));
    });
    step!(300, |st, app, r, fails| {
        check(r, fails, "setting off: nothing on, still on Albums", !app.global::<Autoscroll>().get_on() && app.global::<Nav>().get_page() == Page::Albums, state(app));
        st.settings.borrow_mut().autoscroll = true;
        views::refresh_player_settings(st);
        views::go(st, Page::Settings);
    });
    step!(600, |st, _app, _r, _| views::settings_section(st, crate::settings::nav::WINDOW));
    step!(800, |_st, app, r, fails| {
        snapshot(app, "as-13-settings-window");
        // The window strip along the top: a middle click there scrolls the page under it
        // (here: nothing to scroll), never moves the window.
        move_to(app, 600.0, 10.0);
        let kept = click(app, Button::Middle);
        check(r, fails, "middle click on the top strip kept (no window drag)", kept == (true, true), state(app));
    });
    step!(300, |_st, app, r, fails| {
        autoscroll::away(app);
        let n = fails.get();
        line(r, if n == 0 { "all checks passed".into() } else { format!("{n} checks FAILED") });
        write_report("autoscroll", &r.borrow());
        let _ = slint::quit_event_loop();
    });
    run_steps(steps);
}

// ---- tray --------------------------------------------------------------------------

/// GDI and USER objects held (Windows; 0 elsewhere: `platform::system`).
fn gui_objects() -> (u32, u32) {
    crate::platform::system::gui_objects()
}

/// This process's CPU time (user + kernel; `platform::system`).
fn cpu_time() -> Duration {
    crate::platform::system::cpu_time()
}

/// This process's CPU cycles (all threads), precise where `GetProcessTimes` counts in
/// 15.6 ms ticks.
fn cycles() -> u64 {
    crate::platform::system::cycles().unwrap_or(0)
}

/// The cycle counter's rate (the TSC), measured over 200 ms.
fn cycle_hz() -> f64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: reading the time-stamp counter has no preconditions on x86-64.
        let (t0, i0) = (unsafe { core::arch::x86_64::_rdtsc() }, Instant::now());
        std::thread::sleep(Duration::from_millis(200));
        let (t1, i1) = (unsafe { core::arch::x86_64::_rdtsc() }, Instant::now());
        return (t1 - t0) as f64 / (i1 - i0).as_secs_f64();
    }
    #[allow(unreachable_code)]
    3.0e9
}

fn unix() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Frames side by side at `size` px, each `zoom` times, on `bg`, with a 1x strip under.
fn sheet(frames: &[[f32; 5]], size: u32, zoom: u32, bg: [u8; 3], light: bool) -> image::RgbaImage {
    let n = frames.len() as u32;
    let pad = 4 * zoom;
    let cell = size * zoom + pad;
    let w = n * cell + pad;
    let h = cell + pad + size + 2 * pad;
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([bg[0], bg[1], bg[2], 255]));
    let blend = |img: &mut image::RgbaImage, x: u32, y: u32, p: &[u8]| {
        let a = f32::from(p[3]) / 255.0;
        let d = img.get_pixel_mut(x, y);
        for (dc, &pc) in d.0.iter_mut().zip(p).take(3) {
            *dc = (f32::from(pc) * a + f32::from(*dc) * (1.0 - a)).round() as u8;
        }
    };
    for (k, hts) in frames.iter().enumerate() {
        let px = crate::tray::icon::tray_frame_on(size, hts, light);
        for y in 0..size {
            for x in 0..size {
                let p = &px[((y * size + x) * 4) as usize..][..4];
                for zy in 0..zoom {
                    for zx in 0..zoom {
                        blend(&mut img, pad + k as u32 * cell + x * zoom + zx, pad + y * zoom + zy, p);
                    }
                }
                // Actual size underneath.
                blend(&mut img, pad + k as u32 * (size + pad) + x, cell + pad + y, p);
            }
        }
    }
    img
}

pub fn run_tray() {
    let secs: u64 = std::env::var("MP_BENCH_TRAY_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let idle: u64 = std::env::var("MP_BENCH_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let r: Report = Rc::default();
    let fails: Rc<Cell<u32>> = Rc::default();
    let c0 = Rc::new(Cell::new(Duration::ZERO));
    let w0 = Rc::new(Cell::new(Instant::now()));
    let mut steps: Vec<Step> = Vec::new();
    macro_rules! step {
        ($ms:expr, $f:expr) => {{
            let (r, fails, c0, w0) = (r.clone(), fails.clone(), c0.clone(), w0.clone());
            let f: Box<dyn FnOnce(&Rc<State>, &AppWindow, &Report, &Rc<Cell<u32>>, &Rc<Cell<Duration>>, &Rc<Cell<Instant>>)> = Box::new($f);
            steps.push(($ms, Box::new(move |st: &Rc<State>, app: &AppWindow| f(st, app, &r, &fails, &c0, &w0))));
        }};
    }
    HZ.with(|h| h.set(cycle_hz()));
    let begin = |c0: &Rc<Cell<Duration>>, w0: &Rc<Cell<Instant>>| {
        c0.set(cpu_time());
        CY0.with(|c| c.set(cycles()));
        w0.set(Instant::now());
    };
    let spent = |c0: &Rc<Cell<Duration>>, w0: &Rc<Cell<Instant>>| {
        let cpu = cpu_time() - c0.get();
        let wall = w0.get().elapsed().as_secs_f64();
        let (memory, _, _) = crate::platform::system::resources();
        let ms = cpu.as_secs_f64() * 1000.0;
        if let Some(end) = crate::platform::system::cycles() {
            let busy = (end - CY0.with(Cell::get)) as f64 / HZ.with(Cell::get);
            format!("process CPU {:.1} ms by cycles ({ms:.0} ms by GetProcessTimes) in {wall:.1} s = {:.3}% of one core; memory {memory:.1} MiB", busy * 1000.0, 100.0 * busy / wall)
        } else {
            format!("process CPU {ms:.0} ms in {wall:.1} s = {:.3}% of one core; memory {memory:.1} MiB", 100.0 * cpu.as_secs_f64() / wall)
        }
    };

    step!(500, move |st, app, r, _, c0, w0| {
        let (gdi, user) = gui_objects();
        line(r, format!("tray icon size {} px; pid {}; GDI {gdi}, USER {user} objects", crate::tray::anim::running().2, std::process::id()));
        st.settings.borrow_mut().tray_animation = false;
        // Repeat one: the 90 s track plays through every phase.
        for _ in 0..3 {
            if app.global::<crate::Player>().get_repeat() == 2 {
                break;
            }
            playback::cycle_repeat(st);
        }
        playback::play_tracks(st, album_tracks(st, "Groove"), 0);
        let _ = app.hide();
        begin(c0, w0);
        line(r, format!("phase playing-static (animation off, window hidden): starts at unix {:.1}", unix()));
    });
    // Ten-second marks while playing: GDI and USER objects.
    for _ in 0..(secs / 10).max(1) {
        step!(10_000, |_st, _app, r, _, _, _| {
            let (gdi, user) = gui_objects();
            line(r, format!("  t+10s: GDI {gdi}, USER {user}; {:?}", crate::tray::anim::running()));
        });
    }
    step!(0, move |st, _app, r, fails, c0, w0| {
        line(r, format!("phase playing-static: {}", spent(c0, w0)));
        let (run, _, _) = crate::tray::anim::running();
        check(r, fails, "animation off: no timer", !run, format!("{:?}", crate::tray::anim::running()));
        st.settings.borrow_mut().tray_animation = true;
        crate::tray::anim::record(true);
        crate::tray::anim::wake(st);
        begin(c0, w0);
        line(r, format!("phase playing-animated (window hidden): starts at unix {:.1}", unix()));
    });
    let gdi0 = Rc::new(Cell::new((0u32, 0u32)));
    {
        let gdi0 = gdi0.clone();
        step!(2000, move |_st, _app, r, _, _, _| {
            gdi0.set(gui_objects());
            line(r, format!("  2 s in: GDI {}, USER {}", gdi0.get().0, gdi0.get().1));
        });
    }
    for _ in 0..(secs / 10).max(1) {
        step!(10_000, |_st, _app, r, _, _, _| {
            let (gdi, user) = gui_objects();
            line(r, format!("  t+10s: GDI {gdi}, USER {user}; {:?}", crate::tray::anim::running()));
        });
    }
    {
        let gdi0 = gdi0.clone();
        step!(0, move |st, _app, r, fails, c0, w0| {
            line(r, format!("phase playing-animated: {}", spent(c0, w0)));
            let stats = crate::tray::anim::take_stats();
            let n = stats.len();
            let span = stats.last().zip(stats.first()).map_or(1.0, |(l, f)| (l.0 - f.0).as_secs_f64());
            let sent = stats.iter().filter(|s| s.2).count();
            let mut took: Vec<f64> = stats.iter().map(|s| s.1.as_secs_f64() * 1000.0).collect();
            took.sort_by(f64::total_cmp);
            let pct = |p: f64| took.get(((took.len().max(1) - 1) as f64 * p) as usize).copied().unwrap_or(0.0);
            line(r, format!("  {n} frames in {span:.1} s ({:.1} per s), {sent} icons sent; a frame takes p50 {:.3} ms, p95 {:.3}, max {:.3} (drawing)", n as f64 / span, pct(0.5), pct(0.95), pct(1.0)));
            let (gdi, user) = gui_objects();
            check(r, fails, "no GDI or USER objects leaked", gdi <= gdi0.get().0 + 2 && user <= gdi0.get().1 + 2, format!("GDI {} -> {gdi}, USER {} -> {user} over {sent} icons", gdi0.get().0, gdi0.get().1));
            if !crate::platform::system::TRAY_ANIMATES {
                check(r, fails, "the static status item schedules no frames", n == 0, format!("{n} frames"));
            } else {
                check(r, fails, "the bars move", sent > n / 3, format!("{sent} of {n}"));
            }
            // Contact sheets: 24 frames from the middle of the run.
            let from = n.saturating_sub(48);
            let frames: Vec<[f32; 5]> = stats[from..(from + 24).min(n)].iter().map(|s| s.3).collect();
            for (size, zoom) in [(16, 8), (32, 4)] {
                for (name, bg) in [("dark", [0x20, 0x20, 0x20]), ("light", [0xee, 0xee, 0xee])] {
                    let img = sheet(&frames, size, zoom, bg, name == "light");
                    let _ = img.save(out_dir().join(format!("tray-sheet-{size}-{name}.png")));
                }
            }
            line(r, format!("  heights of the sheet's frames: {:?}", frames.iter().map(|h| h.map(|v| v.round() as i32)).collect::<Vec<_>>()));
            // Pause: the settle.
            crate::tray::anim::record(true);
            playback::set_paused(st, true);
            begin(c0, w0);
        });
    }
    step!(1500, move |_st, _app, r, fails, _, _| {
        let stats = crate::tray::anim::take_stats();
        let (run, sent, _) = crate::tray::anim::running();
        line(r, format!("[pause] {} frames after the pause: {:?}", stats.len(), stats.iter().map(|s| s.3.map(|v| v.round() as i32)).collect::<Vec<_>>()));
        if !crate::platform::system::TRAY_ANIMATES {
            check(r, fails, "the static status item stays at rest", stats.is_empty() && !run, format!("timer {run}"));
        } else {
            check(r, fails, "settles over a few frames, then the timer stops", !run && (2..=8).contains(&stats.len()), format!("timer {run}, icons sent {sent}"));
            check(r, fails, "the last frame is the resting glyph", stats.last().is_some_and(|s| s.3 == crate::tray::icon::REST_HEIGHTS), String::new());
        }
    });
    step!(500, move |st, _app, r, _, c0, w0| {
        begin(c0, w0);
        line(r, format!("phase idle (paused, window hidden): starts at unix {:.1}; engine {:?}", unix(), st.player.status().state));
        write_report("tray", &r.borrow());
    });
    step!(idle * 1000, move |_st, _app, r, _fails, c0, w0| {
        line(r, format!("phase idle: {}; ends at unix {:.1}", spent(c0, w0), unix()));
        let (gdi, user) = gui_objects();
        line(r, format!("GDI {gdi}, USER {user} after the idle phase"));
        write_report("tray", &r.borrow());
    });
    tray_gestures(&mut steps, &r, &fails);
    step!(0, move |_st, _app, r, fails, _, _| {
        let n = fails.get();
        line(r, if n == 0 { "all checks passed".into() } else { format!("{n} checks FAILED") });
        write_report("tray", &r.borrow());
        let _ = slint::quit_event_loop();
    });
    run_steps(steps);
}

/// The tray icon's gestures (`tray/mod.rs`), paused at volume 0, the window hidden: what the
/// shell sends for each is handed to the icon's window inside the app (`tray::simulate`;
/// no real input, the pointer never moves). A click plays or pauses only once the
/// double-click time is over; a double click shows or hides the window and never also
/// plays; the middle button skips; the wheel steps the volume by 5% (only while paused,
/// so nothing is heard), the tooltip saying it at once; on "Show window" a click shows
/// the window at once; the wheel hook is in only when the real pointer is over the icon.
fn tray_gestures(steps: &mut Vec<Step>, r: &Report, fails: &Rc<Cell<u32>>) {
    use crate::platform::TrayPointer as P;
    let dbl = crate::platform::tray::double_click_time();
    let half = (dbl / 2).as_millis() as u64;
    let past = dbl.as_millis() as u64 + 400;
    macro_rules! step {
        ($ms:expr, $f:expr) => {{
            let (r, fails) = (r.clone(), fails.clone());
            let f: Box<dyn FnOnce(&Rc<State>, &AppWindow, &Report, &Rc<Cell<u32>>)> = Box::new($f);
            steps.push(($ms, Box::new(move |st: &Rc<State>, app: &AppWindow| f(st, app, &r, &fails))));
        }};
    }
    fn playing(st: &Rc<State>) -> bool {
        st.player.status().state == player_core::PlayState::Playing
    }
    fn volume(st: &Rc<State>) -> f32 {
        st.settings.borrow().volume
    }
    fn tip() -> String {
        crate::tray::tooltip().unwrap_or_default()
    }
    let acts0 = Rc::new(Cell::new(0u32));
    let index0 = Rc::new(Cell::new(None::<usize>));
    {
        let acts0 = acts0.clone();
        step!(500, move |st, app, r, fails| {
            line(r, format!("phase gestures: double-click time {} ms", dbl.as_millis()));
            st.settings.borrow_mut().tray_click = crate::settings::TrayClick::PlayPause;
            acts0.set(crate::tray::acts());
            check(r, fails, "paused, the window hidden, volume 0", !playing(st) && !app.window().is_visible() && volume(st) == 0.0, format!("engine {:?}, volume {}", st.player.status().state, volume(st)));
            let sent = crate::tray::simulate(P::Click);
            check(r, fails, "the icon's messages reach the app", sent, String::new());
        });
    }
    step!(half, move |st, _app, r, fails| {
        check(r, fails, "a click waits for the double-click time", !playing(st), format!("{:?}", st.player.status().state));
    });
    {
        let acts0 = acts0.clone();
        step!(past - half, move |st, _app, r, fails| {
            check(r, fails, "then it plays (as the Play button would)", playing(st) && crate::tray::acts() == acts0.get() + 1, format!("{:?}, {} acts", st.player.status().state, crate::tray::acts() - acts0.get()));
            crate::tray::simulate(P::Click);
        });
    }
    step!(past, move |st, _app, r, fails| {
        check(r, fails, "a second click pauses", !playing(st), format!("{:?}", st.player.status().state));
        crate::tray::simulate(P::DoubleClick);
    });
    step!(300, move |_st, app, r, fails| {
        check(r, fails, "a double click shows the window at once", app.window().is_visible(), String::new());
    });
    {
        let acts0 = acts0.clone();
        step!(past, move |st, _app, r, fails| {
            check(r, fails, "and never also plays", !playing(st) && crate::tray::acts() == acts0.get() + 3, format!("{:?}, {} acts", st.player.status().state, crate::tray::acts() - acts0.get()));
            crate::tray::simulate(P::DoubleClick);
        });
    }
    {
        let index0 = index0.clone();
        step!(past, move |st, app, r, fails| {
            check(r, fails, "a double click on the shown window hides it", !app.window().is_visible() && !playing(st), format!("{:?}", st.player.status().state));
            index0.set(st.queue.borrow().current_index());
            crate::tray::simulate(P::Middle);
        });
    }
    {
        let index0 = index0.clone();
        step!(700, move |st, _app, r, fails| {
            let now = st.queue.borrow().current_index();
            check(r, fails, "the middle button plays the next track", now.is_some() && now != index0.get(), format!("{:?} -> {now:?}", index0.get()));
            playback::set_paused(st, true);
        });
    }
    step!(500, move |st, _app, r, fails| {
        let quiet = !playing(st) && volume(st) == 0.0;
        check(r, fails, "paused again at volume 0 before the wheel", quiet, format!("{:?}, volume {}", st.player.status().state, volume(st)));
        if quiet {
            crate::tray::simulate(P::Wheel(120));
        }
    });
    step!(300, move |st, _app, r, fails| {
        check(r, fails, "a notch up: 5%, said in the tooltip", (volume(st) - 0.05).abs() < 1e-6 && tip().starts_with("Volume 5%\n"), format!("volume {}, tooltip {:?}", volume(st), tip()));
        check(r, fails, "still paused", !playing(st), String::new());
        crate::tray::simulate(P::Wheel(-60));
        crate::tray::simulate(P::Wheel(-60));
    });
    step!(300, move |st, _app, r, fails| {
        check(r, fails, "two half notches down: back to 0", volume(st) == 0.0 && tip().starts_with("Volume 0%\n"), format!("volume {}, tooltip {:?}", volume(st), tip()));
        crate::tray::simulate(P::Wheel(-120));
    });
    step!(300, move |st, _app, r, fails| {
        check(r, fails, "down at 0 stays at 0", volume(st) == 0.0, format!("volume {}", volume(st)));
    });
    step!(3500, move |st, _app, r, fails| {
        check(r, fails, "the tooltip drops the volume after a few seconds", !tip().starts_with("Volume"), format!("{:?}", tip()));
        st.settings.borrow_mut().tray_click = crate::settings::TrayClick::ShowWindow;
        crate::tray::simulate(P::Click);
    });
    step!(250, move |st, app, r, fails| {
        check(r, fails, "on Show window, a click shows the window at once", app.window().is_visible() && !playing(st), format!("{:?}", st.player.status().state));
        crate::tray::simulate(P::DoubleClick);
    });
    step!(past, move |st, app, r, fails| {
        check(r, fails, "and a double click leaves it shown, still paused", app.window().is_visible() && !playing(st), format!("{:?}", st.player.status().state));
        st.settings.borrow_mut().tray_click = crate::settings::TrayClick::PlayPause;
        let _ = app.hide();
        crate::tray::simulate(P::Hover);
    });
    step!(400, move |_st, _app, r, fails| {
        // The real pointer is wherever the person at the computer left it (never moved).
        let over = crate::platform::tray::pointer_over_icon();
        let hooked = crate::platform::tray::wheel_hooked();
        check(r, fails, "the wheel hook is in only with the pointer over the icon", hooked == over.unwrap_or(false), format!("pointer over the icon {over:?}, hook in {hooked}"));
    });
}
