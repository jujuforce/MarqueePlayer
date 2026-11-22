//! `MP_BENCH=viz-styles`: every visualizer preset in every placement, the colour schemes,
//! the minimum window, Settings > Visualizer, Now playing's visualizer menu, moments of the
//! realistic fixtures and a strip of consecutive frames; `vs-*.png` and checks in
//! `bench-viz-styles.txt` (a count of failed checks at its end).
//!
//! `MP_BENCH=viz-cost`: each style in each placement for `MP_BENCH_VIZ_SECS` (default 5)
//! seconds: frames, frame intervals, the work per frame and the process CPU
//! (`bench-viz-cost.txt`); then 30 frames a second, and the window redrawing a 1 px
//! rectangle per frame (`MP_VIZ_DRAW=minimal` is the outside switch for that).
//!
//! Both need `MP_MUSIC` with the fixtures of `examples/viz_fixtures` (the albums "Night
//! Shift", "Daylight" and "Spectrum"). `MP_BENCH_VIZ_PART` (comma-separated: presets,
//! colours, min, settings, menu, moments, sheet, idle) runs only those parts of
//! `viz-styles`. Events go through Slint's own dispatch inside the app; no OS input.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::bench::{Step, menu_labels, out_dir, press, run_steps, snapshot, tabs, write_report};
use crate::state::State;
use crate::visualizer::look::{Placement, Preset};
use crate::{AppWindow, MenuStore, Nav, Page, Visualizer, playback, settings, views};

type Report = Rc<RefCell<String>>;

fn line(r: &Report, text: String) {
    log::info!("viz bench: {text}");
    r.borrow_mut().push_str(&text);
    r.borrow_mut().push('\n');
}

thread_local! {
    static FAILS: Cell<u32> = const { Cell::new(0) };
}

fn check(r: &Report, what: &str, ok: bool, detail: String) {
    if !ok {
        FAILS.with(|f| f.set(f.get() + 1));
        crate::bench::count_fail();
    }
    line(r, format!("  [{}] {what}: {detail}", if ok { "ok" } else { "FAIL" }));
}

/// This process's CPU time (user + kernel; `platform::system`).
pub(crate) fn cpu_time() -> Duration {
    crate::platform::system::cpu_time()
}

fn album(st: &Rc<State>, title: &str) -> Vec<library::Track> {
    let id: Option<i64> = st.db.query_row("SELECT id FROM albums WHERE title = ?1", [title], |r| r.get(0)).ok();
    id.map(|a| library::query::album_tracks(&st.db, a).unwrap_or_default()).unwrap_or_default()
}

fn size_window(st: &State, w: i32, h: i32) {
    crate::platform::devwindow::resize_client(st.hwnd.get(), w, h);
}

fn part(name: &str) -> bool {
    match std::env::var("MP_BENCH_VIZ_PART") {
        Ok(v) if !v.trim().is_empty() => v.split(',').any(|p| p.trim() == name),
        _ => true,
    }
}

fn placement_name(p: Placement) -> &'static str {
    match p {
        Placement::Behind => "behind",
        Placement::Panel => "panel",
        Placement::Page => "page",
    }
}

/// Show a preset in a placement (the panel's tab chosen for the panel).
fn show(st: &Rc<State>, app: &AppWindow, preset: Preset, place: Placement) {
    let i = Preset::ALL.iter().position(|p| *p == preset).unwrap_or(0);
    crate::visualizer::choose_preset(st, i);
    let pi = Placement::ALL.iter().position(|p| *p == place).unwrap_or(0);
    crate::visualizer::choose(st, "placement", pi);
    app.global::<Visualizer>().set_tab(place == Placement::Panel);
    crate::visualizer::wake(st);
}

fn state(app: &AppWindow) -> String {
    let (timer, tap) = crate::visualizer::running();
    format!("timer {timer}, tap {tap}, drawing {}", app.global::<Visualizer>().get_active())
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn frame_report(name: &str, stats: &[(Instant, Duration)], cpu: Duration, wall: Duration) -> String {
    let mut iv: Vec<f64> = stats.windows(2).map(|w| (w[1].0 - w[0].0).as_secs_f64() * 1000.0).collect();
    let mut work: Vec<f64> = stats.iter().map(|s| s.1.as_secs_f64() * 1000.0).collect();
    iv.sort_by(f64::total_cmp);
    work.sort_by(f64::total_cmp);
    format!(
        "{name:<34} {:>4} frames {:>5.1} fps | interval p50 {:>5.2} p95 {:>5.2} max {:>6.2} ms | work p50 {:.3} p95 {:.3} ms | CPU {:>5.1}% of a core",
        stats.len(),
        stats.len() as f64 / wall.as_secs_f64().max(1e-9),
        pct(&iv, 0.5),
        pct(&iv, 0.95),
        iv.last().copied().unwrap_or(0.0),
        pct(&work, 0.5),
        pct(&work, 0.95),
        100.0 * cpu.as_secs_f64() / wall.as_secs_f64().max(1e-9),
    )
}

pub fn run() {
    let r: Report = Rc::default();
    let mut steps: Vec<Step> = Vec::new();
    macro_rules! step {
        ($ms:expr, $f:expr) => {{
            let r = r.clone();
            let f: Box<dyn FnOnce(&Rc<State>, &AppWindow, &Report)> = Box::new($f);
            steps.push(($ms, Box::new(move |st: &Rc<State>, app: &AppWindow| f(st, app, &r))));
        }};
    }
    macro_rules! shot {
        ($ms:expr, $name:expr) => {{
            let name: String = $name;
            steps.push(($ms, Box::new(move |_st: &Rc<State>, app: &AppWindow| {
                snapshot(app, &name);
            })));
        }};
    }

    step!(1200, |st, app, r| {
        let s = app.window().size().to_logical(app.window().scale_factor());
        line(r, format!("window {}x{} logical; reduced motion {}", s.width, s.height, crate::visualizer::reduced_motion()));
        let n = album(st, "Night Shift");
        check(r, "fixtures", n.len() == 3 && album(st, "Daylight").len() == 3, format!("Night Shift {} tracks", n.len()));
        // With Windows' animations off (or MP_REDUCED_MOTION=1), the default is Calm.
        let want = if std::env::var("MP_REDUCED_MOTION").map(|v| v == "1").unwrap_or_else(|_| crate::visualizer::reduced_motion()) { "calm" } else { "classic" };
        check(r, &format!("on by default, {want}, behind"), st.settings.borrow().visualizer && crate::visualizer::settings(st).preset == want, format!("{:?}", crate::visualizer::settings(st)));
        playback::play_tracks(st, n, 0);
        views::go(st, Page::NowPlaying);
    });

    // ---- Every preset in every placement, 1080p -------------------------------------------
    if part("presets") {
        for place in Placement::ALL {
            for preset in Preset::ALL {
                step!(200, move |st, app, _r| {
                    show(st, app, preset, place);
                    playback::seek(st, 6.0);
                });
                shot!(1300, format!("vs-{}-{}-a", placement_name(place), preset.id()));
                shot!(450, format!("vs-{}-{}-b", placement_name(place), preset.id()));
            }
        }
        step!(100, |st, app, r| {
            show(st, app, Preset::Classic, Placement::Behind);
            check(r, "running behind the page", crate::visualizer::running() == (true, true), state(app));
        });
    }

    // ---- Colour schemes, by height and across, on the full page -------------------------
    if part("colours") {
        for (i, by) in [(0usize, 0usize), (1, 0), (2, 0), (3, 0), (4, 1), (5, 0), (6, 0), (7, 0), (8, 0), (1, 1), (3, 1)] {
            step!(150, move |st, app, _r| {
                show(st, app, Preset::Classic, Placement::Page);
                crate::visualizer::choose(st, "colours", i);
                crate::visualizer::choose(st, "colour_by", by);
                if i == 8 {
                    crate::visualizer::custom_step(st, 0, 3);
                }
            });
            shot!(900, format!("vs-colour-{i}-{}", if by == 0 { "height" } else { "across" }));
        }
        step!(100, |_st, app, r| {
            let p = crate::visualizer::palette();
            check(r, "the cover's palette", p.len() >= 2, format!("{p:?}"));
            let _ = app;
        });
        // The line across, mirrored, with the reflection's strips; the bars across in a rainbow.
        step!(150, |st, app, _r| {
            show(st, app, Preset::Prism, Placement::Page);
            crate::visualizer::choose(st, "colour_by", 1);
            crate::visualizer::toggle(st, "mirror");
        });
        shot!(900, "vs-line-across-mirror".into());
        step!(150, |st, app, _r| {
            show(st, app, Preset::Classic, Placement::Page);
            crate::visualizer::choose(st, "colours", 4);
            crate::visualizer::choose(st, "colour_by", 1);
        });
        shot!(900, "vs-bars-rainbow-across".into());
    }

    // ---- Moments of the realistic fixtures ------------------------------------------------
    if part("moments") {
        let moments: [(&'static str, usize, f64, &'static str); 8] = [
            ("Night Shift", 0, 3.0, "house-3s"),
            ("Night Shift", 0, 20.2, "house-20s"),
            ("Night Shift", 1, 5.0, "hiphop-5s"),
            ("Night Shift", 2, 3.0, "dynamics-intro"),
            ("Night Shift", 2, 17.0, "dynamics-drop"),
            ("Daylight", 0, 6.0, "rock-6s"),
            ("Daylight", 1, 8.0, "ballad-8s"),
            ("Daylight", 2, 6.0, "ambient-6s"),
        ];
        for (preset, place) in [(Preset::Classic, Placement::Behind), (Preset::Prism, Placement::Page), (Preset::Bonfire, Placement::Page)] {
            for (al, track, at, name) in moments {
                step!(150, move |st, app, _r| {
                    show(st, app, preset, place);
                    playback::play_tracks(st, album(st, al), track);
                    playback::seek(st, at);
                });
                shot!(1600, format!("vs-moment-{}-{name}", preset.id()));
            }
        }
        step!(100, |st, _app, _r| playback::play_tracks(st, album(st, "Night Shift"), 0));
    }

    // ---- A strip of consecutive frames ----------------------------------------------------
    if part("sheet") {
        for (preset, name) in [(Preset::Classic, "classic"), (Preset::Prism, "prism"), (Preset::Halo, "halo")] {
            let frames: Rc<RefCell<Vec<(image::RgbaImage, u128, i32)>>> = Rc::default();
            let t0 = Rc::new(Cell::new(Instant::now()));
            {
                let t0 = t0.clone();
                step!(150, move |st, app, _r| {
                    show(st, app, preset, Placement::Page);
                    playback::play_tracks(st, album(st, "Night Shift"), 0);
                    playback::seek(st, 8.0);
                    t0.set(Instant::now());
                });
            }
            // A snapshot takes a few ms: one every 17 ms catches each frame once (60 a second).
            for k in 0..12 {
                let (frames, t0) = (frames.clone(), t0.clone());
                steps.push((if k == 0 { 1500 } else { 17 }, Box::new(move |_st, app| {
                    if let Some(buf) = super::window_pixels(app)
                        && let Some(img) = image::RgbaImage::from_raw(buf.width(), buf.height(), buf.as_bytes().to_vec())
                    {
                        frames.borrow_mut().push((img, t0.get().elapsed().as_millis(), app.global::<Visualizer>().get_frame()));
                    }
                })));
            }
            step!(10, move |_st, _app, r| {
                let f = frames.borrow();
                let Some((first, _, _)) = f.first() else { return };
                let (w, h) = (first.width(), first.height());
                // The stage's band of each frame, half size, one under another.
                let (y0, y1) = (h * 19 / 100, h - h * 9 / 100);
                let (sw, sh) = (w / 2, (y1 - y0) / 2);
                let mut sheet = image::RgbaImage::new(sw, sh * f.len() as u32);
                for (i, (img, _, _)) in f.iter().enumerate() {
                    let crop = image::imageops::crop_imm(img, 0, y0, w, y1 - y0).to_image();
                    let small = image::imageops::resize(&crop, sw, sh, image::imageops::FilterType::Triangle);
                    image::imageops::replace(&mut sheet, &small, 0, i64::from(sh) * i as i64);
                }
                let path = out_dir().join(format!("vs-sheet-{name}.png"));
                let _ = sheet.save(&path);
                let times: Vec<String> = f.iter().map(|(_, t, fr)| format!("{t} ms (frame {fr})")).collect();
                line(r, format!("[sheet] {name}: {}", times.join(", ")));
            });
        }
    }

    // ---- The minimum window -----------------------------------------------------------------
    if part("min") {
        step!(200, |st, _app, _r| size_window(st, 960, 620));
        for place in Placement::ALL {
            for preset in [Preset::Classic, Preset::Prism, Preset::Halo, Preset::Bonfire] {
                step!(150, move |st, app, _r| show(st, app, preset, place));
                shot!(1100, format!("vs-min-{}-{}", placement_name(place), preset.id()));
            }
        }
        // The sidebar and the queue pinned.
        step!(150, |st, app, _r| {
            show(st, app, Preset::Prism, Placement::Page);
            app.global::<Nav>().set_sidebar_pinned(true);
            app.global::<Nav>().set_queue_pinned(true);
        });
        shot!(1200, "vs-min-page-pinned".into());
        step!(150, |st, app, _r| show(st, app, Preset::Classic, Placement::Panel));
        shot!(1100, "vs-min-panel-pinned".into());
        step!(150, |st, app, _r| {
            show(st, app, Preset::Classic, Placement::Behind);
            app.global::<Nav>().set_sidebar_pinned(false);
            app.global::<Nav>().set_queue_pinned(false);
            // Auto-hide on: on Now playing the bar stays, as before.
            st.settings.borrow_mut().bar_autohide = true;
            views::refresh_player_settings(st);
        });
        shot!(1200, "vs-min-behind-autohide".into());
        step!(150, |st, _app, _r| {
            st.settings.borrow_mut().bar_autohide = false;
            views::refresh_player_settings(st);
            size_window(st, 1920, 1032);
        });
        step!(1200, |_st, _app, _r| {});
    }

    // ---- Now playing's menu ---------------------------------------------------------------
    if part("menu") {
        step!(150, |st, app, _r| {
            show(st, app, Preset::Classic, Placement::Behind);
            crate::visualizer::menu(st, 1600.0, 80.0, false);
        });
        step!(500, |_st, app, r| {
            snapshot(app, "vs-menu");
            let l = menu_labels(app);
            check(r, "the menu", l.first().map(String::as_str) == Some("Visualizer") && l.iter().any(|x| x == "Prism") && l.iter().any(|x| x == "Full page"), format!("{l:?}"));
            check(r, "the presets under their heading, no Style submenu", l.iter().any(|x| x == "Presets") && !l.iter().any(|x| x.starts_with("Style")), format!("{l:?}"));
            // Down to the opacity's row (after Visualizer), Right opens its steps.
            press(app, slint::platform::Key::DownArrow, &[]);
            press(app, slint::platform::Key::DownArrow, &[]);
            press(app, slint::platform::Key::RightArrow, &[]);
        });
        step!(400, |_st, app, r| {
            snapshot(app, "vs-menu-opacity");
            let m = app.global::<MenuStore>();
            check(r, "Right opens the opacity's steps", m.get_sub_of() == 1, format!("sub of {}", m.get_sub_of()));
            // Left closes them; Down passes over the separator and the "Presets" heading to
            // Classic, then Prism, Skyline and Calm (a line); Enter.
            press(app, slint::platform::Key::LeftArrow, &[]);
            for _ in 0..4 {
                press(app, slint::platform::Key::DownArrow, &[]);
            }
            press(app, slint::platform::Key::Return, &[]);
        });
        step!(500, |st, app, r| {
            let s = crate::visualizer::settings(st);
            let l = s.effective(&crate::visualizer::look::ThemeViz::default());
            check(
                r,
                "Calm, a line, chosen from the menu's presets by the keys",
                s.preset == Preset::Calm.id() && l.style == crate::visualizer::look::Style::Line && !app.global::<MenuStore>().get_open(),
                format!("{} ({:?})", s.preset, l.style),
            );
            snapshot(app, "vs-menu-line-chosen");
            // The C key: the next preset.
            crate::bench::type_text(app, "c");
        });
        step!(500, |st, app, r| {
            let s = crate::visualizer::settings(st);
            check(r, "C: the next preset", s.preset == "classic" || Preset::from_id(&s.preset).is_some(), format!("{} ({:?})", s.preset, app.global::<Nav>().get_flash().to_string()));
            crate::bench::type_text(app, "v");
        });
        step!(500, |st, app, r| {
            check(r, "V: off, and nothing runs", !st.settings.borrow().visualizer && crate::visualizer::running() == (false, false), state(app));
            snapshot(app, "vs-off");
            crate::bench::type_text(app, "v");
        });
        step!(600, |st, app, r| {
            check(r, "V again: on", st.settings.borrow().visualizer && crate::visualizer::running().0, state(app));
            // The panel's tab: Y gives way to the lyrics, and nothing runs on Up next.
            show(st, app, Preset::Classic, Placement::Panel);
        });
        step!(600, |st, app, r| {
            check(r, "the panel's tab runs", crate::visualizer::running().0, state(app));
            crate::bench::type_text(app, "y");
            let _ = st;
        });
        step!(600, |_st, app, r| {
            check(r, "Y: lyrics, nothing runs", !app.global::<Visualizer>().get_tab() && app.global::<crate::Lyrics>().get_shown() && !crate::visualizer::running().0, state(app));
            snapshot(app, "vs-panel-lyrics");
            app.global::<crate::Lyrics>().set_shown(false);
        });
        step!(600, |_st, app, r| {
            check(r, "Up next: nothing runs", !crate::visualizer::running().0, state(app));
            snapshot(app, "vs-panel-upnext");
        });
    }

    // ---- Settings > Visualizer ----------------------------------------------------------------
    if part("settings") {
        step!(150, |st, app, _r| {
            show(st, app, Preset::Classic, Placement::Behind);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        step!(1200, |_st, app, r| {
            snapshot(app, "vs-settings-playing");
            check(r, "the preview runs while playing", crate::visualizer::running().0 && !app.global::<Visualizer>().get_still(), state(app));
        });
        step!(100, |st, _app, _r| playback::set_paused(st, true));
        step!(2500, |_st, app, r| {
            snapshot(app, "vs-settings-still");
            check(r, "paused: a still frame, nothing running", !crate::visualizer::running().0 && app.global::<Visualizer>().get_still() && app.global::<Visualizer>().get_active(), state(app));
        });
        for (i, name) in [(1usize, "prism"), (2, "halo"), (3, "bonfire")] {
            step!(150, move |st, _app, _r| crate::visualizer::choose_preset(st, i));
            shot!(500, format!("vs-settings-still-{name}"));
        }
        step!(150, |st, _app, _r| {
            crate::visualizer::choose(st, "colours", 8);
        });
        shot!(500, "vs-settings-custom".into());
        step!(150, |_st, app, _r| app.global::<Nav>().set_page_scroll(-560.0));
        shot!(500, "vs-settings-scrolled".into());
        step!(150, |_st, app, _r| app.global::<Nav>().set_page_scroll(-1400.0));
        shot!(500, "vs-settings-scrolled-2".into());
        // The keyboard: Tab from the filter box, past the list, to the switch, then Where,
        // then the presets; Right picks the next.
        step!(150, |st, app, _r| {
            app.global::<Nav>().set_page_scroll(0.0);
            crate::visualizer::choose_preset(st, 0);
            views::settings_section(st, settings::nav::VISUALIZER);
            app.invoke_focus_shortcuts();
        });
        // The filter box, the section list, the switch, Where, Opacity, the presets.
        step!(300, |_st, app, _r| tabs(app, 6));
        step!(300, |st, app, r| {
            snapshot(app, "vs-settings-keys-presets");
            press(app, slint::platform::Key::RightArrow, &[]);
            let _ = (st, r);
        });
        step!(400, |st, app, r| {
            let s = crate::visualizer::settings(st);
            check(r, "Tab to the presets, Right: Prism", s.preset == "prism", s.preset.clone());
            snapshot(app, "vs-settings-keys-prism");
        });
        step!(150, |st, _app, _r| crate::visualizer::reset(st));
        step!(400, |st, app, r| {
            check(r, "Reset: Classic, behind", crate::visualizer::settings(st).preset == "classic" && app.global::<Visualizer>().get_placement() == 0, format!("{:?}", crate::visualizer::settings(st)));
            views::go(st, Page::Home);
        });
        step!(500, |_st, app, r| {
            check(r, "leaving Settings: nothing runs, nothing drawn", crate::visualizer::running() == (false, false) && !app.global::<Visualizer>().get_active(), state(app));
        });
        // The minimum size.
        step!(150, |st, _app, _r| {
            size_window(st, 960, 620);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        shot!(1200, "vs-settings-min".into());
        step!(150, |st, _app, _r| {
            size_window(st, 1920, 1032);
            playback::set_paused(st, false);
            views::go(st, Page::NowPlaying);
        });
        step!(1000, |_st, _app, _r| {});
    }

    // ---- Idle ---------------------------------------------------------------------------------
    if part("idle") {
        step!(150, |st, app, _r| {
            show(st, app, Preset::Prism, Placement::Page);
            playback::set_paused(st, true);
        });
        step!(2500, |_st, app, r| check(r, "paused: at rest, stopped", crate::visualizer::running() == (false, false) && !app.global::<Visualizer>().get_active(), state(app)));
        step!(100, |st, _app, _r| playback::set_paused(st, false));
        step!(800, |st, app, r| {
            check(r, "playing again: running", crate::visualizer::running() == (true, true), state(app));
            views::go(st, Page::Home);
        });
        step!(300, |_st, app, r| check(r, "another page: stopped at once", crate::visualizer::running() == (false, false), state(app)));
        step!(100, |st, _app, _r| views::go(st, Page::NowPlaying));
        step!(600, |_st, app, r| {
            check(r, "back: running", crate::visualizer::running().0, state(app));
            let _ = app.window().hide();
        });
        step!(400, |_st, app, r| {
            check(r, "hidden: stopped", crate::visualizer::running() == (false, false), state(app));
            let _ = app.window().show();
        });
        step!(600, |_st, app, r| check(r, "shown again: running", crate::visualizer::running().0, state(app)));
    }

    step!(300, |_st, _app, r| {
        line(r, format!("failed checks: {}", FAILS.with(Cell::get)));
        write_report("viz-styles", &r.borrow());
        let _ = slint::quit_event_loop();
    });
    run_steps(steps);
}

pub fn run_cost() {
    let secs: u64 = std::env::var("MP_BENCH_VIZ_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let r: Report = Rc::default();
    let mut steps: Vec<Step> = Vec::new();
    let t0 = Rc::new(Cell::new((Instant::now(), Duration::ZERO)));
    steps.push((1200, Box::new(|st, _app| {
        playback::play_tracks(st, album(st, "Night Shift"), 0);
        views::go(st, Page::NowPlaying);
    })));
    // Playing on Now playing with the visualizer off: the floor.
    {
        let ta = t0.clone();
        steps.push((800, Box::new(move |st, _app| {
            if st.settings.borrow().visualizer {
                crate::toggle_visualizer(st);
            }
            ta.set((Instant::now(), cpu_time()));
        })));
        let (t0, r) = (t0.clone(), r.clone());
        steps.push((secs * 1000, Box::new(move |st, _app| {
            let (s, c) = t0.get();
            line(&r, format!("{:<34} CPU {:>5.1}% of a core", "visualizer off, playing", 100.0 * (cpu_time() - c).as_secs_f64() / s.elapsed().as_secs_f64()));
            crate::toggle_visualizer(st);
        })));
    }
    // A preset, or a kind of ring chosen as Settings does it (`("ring", index in
    // Style::RING)`, of the Halo preset).
    type Case = (Preset, Option<(&'static str, usize)>, Placement, &'static str);
    let cases: Vec<Case> = vec![
        (Preset::Classic, None, Placement::Behind, "bars, behind"),
        (Preset::Prism, None, Placement::Behind, "line, behind"),
        (Preset::Halo, None, Placement::Behind, "halo, behind"),
        (Preset::Bonfire, None, Placement::Behind, "bonfire, behind"),
        (Preset::Halo, Some(("ring", 1)), Placement::Behind, "saturn, behind"),
        (Preset::Halo, Some(("ring", 2)), Placement::Behind, "galaxy, behind"),
        (Preset::Halo, Some(("ring", 3)), Placement::Behind, "swarm, behind"),
        (Preset::Halo, Some(("ring", 4)), Placement::Behind, "eclipse, behind"),
        (Preset::Classic, None, Placement::Panel, "bars, panel"),
        (Preset::Prism, None, Placement::Panel, "line, panel"),
        (Preset::Halo, None, Placement::Panel, "halo, panel"),
        (Preset::Bonfire, None, Placement::Panel, "bonfire, panel"),
        (Preset::Halo, Some(("ring", 1)), Placement::Panel, "saturn, panel"),
        (Preset::Halo, Some(("ring", 2)), Placement::Panel, "galaxy, panel"),
        (Preset::Halo, Some(("ring", 3)), Placement::Panel, "swarm, panel"),
        (Preset::Halo, Some(("ring", 4)), Placement::Panel, "eclipse, panel"),
        (Preset::Classic, None, Placement::Page, "bars, full page"),
        (Preset::Prism, None, Placement::Page, "line, full page"),
        (Preset::Halo, None, Placement::Page, "halo, full page"),
        (Preset::Bonfire, None, Placement::Page, "bonfire, full page"),
        (Preset::Halo, Some(("ring", 1)), Placement::Page, "saturn, full page"),
        (Preset::Halo, Some(("ring", 2)), Placement::Page, "galaxy, full page"),
        (Preset::Halo, Some(("ring", 3)), Placement::Page, "swarm, full page"),
        (Preset::Halo, Some(("ring", 4)), Placement::Page, "eclipse, full page"),
        (Preset::Skyline, None, Placement::Page, "fine mirrored bars, full page"),
    ];
    let only = std::env::var("MP_BENCH_VIZ_CASES").unwrap_or_default();
    let cases: Vec<_> = cases.into_iter().filter(|c| only.is_empty() || only.split(',').any(|o| c.3.contains(o.trim()))).collect();
    for (preset, style, place, name) in cases {
        steps.push((300, Box::new(move |st, app| {
            // From the top of the album each time: the music never runs out mid-case.
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            playback::seek(st, 4.0);
            show(st, app, preset, place);
            if let Some((what, i)) = style {
                crate::visualizer::choose(st, what, i);
            }
        })));
        let ta = t0.clone();
        steps.push((700, Box::new(move |_st, _app| {
            crate::visualizer::record(true);
            ta.set((Instant::now(), cpu_time()));
        })));
        let (t0, r) = (t0.clone(), r.clone());
        steps.push((secs * 1000, Box::new(move |_st, _app| {
            let (s, c) = t0.get();
            let stats = crate::visualizer::take_stats();
            crate::visualizer::record(false);
            line(&r, frame_report(name, &stats, cpu_time() - c, s.elapsed()));
        })));
    }
    // 30 frames a second, the heaviest case and the default.
    for (preset, place, name) in [(Preset::Prism, Placement::Page, "line, full page, 30 fps"), (Preset::Classic, Placement::Behind, "bars, behind, 30 fps")] {
        steps.push((300, Box::new(move |st, app| {
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            playback::seek(st, 4.0);
            show(st, app, preset, place);
            crate::visualizer::choose(st, "fps", 1);
        })));
        let ta = t0.clone();
        steps.push((700, Box::new(move |_st, _app| {
            crate::visualizer::record(true);
            ta.set((Instant::now(), cpu_time()));
        })));
        let (t0, r) = (t0.clone(), r.clone());
        steps.push((secs * 1000, Box::new(move |st, _app| {
            let (s, c) = t0.get();
            let stats = crate::visualizer::take_stats();
            crate::visualizer::record(false);
            line(&r, frame_report(name, &stats, cpu_time() - c, s.elapsed()));
            crate::visualizer::choose(st, "fps", 0);
        })));
    }
    {
        let r = r.clone();
        steps.push((300, Box::new(move |_st, _app| {
            write_report("viz-cost", &r.borrow());
            let _ = slint::quit_event_loop();
        })));
    }
    run_steps(steps);
}
