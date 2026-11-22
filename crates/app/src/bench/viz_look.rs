//! `MP_BENCH=viz-look`: the visualizer's newer parts (`bench-viz-look.txt`, `vl-*.png` at
//! 1920 x 1080 and 960 x 620). Events go through Slint's own dispatch inside the app; no OS
//! input. The clipboard is the bench's stand-in (`explorer::bench_set_clipboard`); no native
//! file dialog opens (export and import take a path in the bench's output folder).
//!
//! - The halo: around the cover in every placement, under three covers (dark: "Night Shift",
//!   bright: "Paper", nearly monochrome: "Graphite"), at both sizes; the hub where the cover
//!   is. `MP_HALO_SHAPE=square` (outside) draws it after the cover's rounded square instead,
//!   for comparing (`vl-halo-square-*`).
//! - The pinned preview of Settings > Visualizer: beside the column on a wide page, held at
//!   the column's top on a narrow one, while the column scrolls (by the wheel's offset and by
//!   Tab), drawn once, still while nothing plays; "Find a setting" over it; the equalizer's
//!   section without it.
//! - The placement key (Shift+V): the cycle, the panel's tab, the pill, with the visualizer
//!   off, on another page; V still turns it on and off; Settings > Keys lists it.
//! - Opacity: Settings' stepper by the keyboard and the pointer, behind the page and
//!   elsewhere apart, the quick menu's submenu by the keyboard.
//! - The user's presets: Save as (by the keyboard), the picker, the look key into them,
//!   Rename, Update, Copy (the stand-in clipboard), Paste (shown first, then Add, Replace or
//!   Keep both, by the keyboard and the pointer), a refused paste (not a preset, a newer
//!   format, a hostile one), Export and Import (a file: shown, cancelled by Esc, added),
//!   Delete; settings.json read back after each.
//!
//! `MP_BENCH_LOOK_PART` (comma-separated: halo, pin, keys, opacity, presets) runs only those.
//! Needs `MP_MUSIC` with the fixtures of `examples/viz_fixtures`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};

use crate::bench::{Step, menu_labels, out_dir, press, run_steps, snapshot, tabs, type_text, write_report};
use crate::state::State;
use crate::visualizer::look::{Placement, Preset, Style, ThemeViz};
use crate::{AppWindow, Dialog, Keys, MenuStore, Nav, Page, SettingsView, Visualizer, playback, settings, views};

type Report = Rc<RefCell<String>>;

fn line(r: &Report, text: String) {
    log::info!("viz-look bench: {text}");
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

fn part(name: &str) -> bool {
    match std::env::var("MP_BENCH_LOOK_PART") {
        Ok(v) if !v.trim().is_empty() => v.split(',').any(|p| p.trim() == name),
        _ => true,
    }
}

fn album(st: &Rc<State>, title: &str) -> Vec<library::Track> {
    let id: Option<i64> = st.db.query_row("SELECT id FROM albums WHERE title = ?1", [title], |r| r.get(0)).ok();
    id.map(|a| library::query::album_tracks(&st.db, a).unwrap_or_default()).unwrap_or_default()
}

fn size_window(st: &State, w: i32, h: i32) {
    crate::platform::devwindow::resize_client(st.hwnd.get(), w, h);
}

fn place_name(p: Placement) -> &'static str {
    match p {
        Placement::Behind => "behind",
        Placement::Panel => "panel",
        Placement::Page => "page",
    }
}

/// A preset (by `Preset::ALL`) in a placement, the panel's tab chosen for the panel.
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
    let g = app.global::<Visualizer>();
    format!("timer {timer}, tap {tap}, drawing {}, still {}, stage {:.0}x{:.0} hub {:.0}", g.get_active(), g.get_still(), g.get_stage_width(), g.get_stage_height(), g.get_stage_hub())
}

fn flash(app: &AppWindow) -> String {
    app.global::<Nav>().get_flash().to_string()
}

/// A key with Shift (or Ctrl) held, through Slint's dispatch, as the keyboard sends it.
fn chord(app: &AppWindow, modifier: Key, ch: &str) {
    let w = app.window();
    w.dispatch_event(WindowEvent::KeyPressed { text: modifier.into() });
    w.dispatch_event(WindowEvent::KeyPressed { text: ch.into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: ch.into() });
    w.dispatch_event(WindowEvent::KeyReleased { text: modifier.into() });
}

fn clip() -> String {
    crate::explorer::bench_clipboard().unwrap_or_default()
}

fn users(st: &State) -> Vec<String> {
    st.settings.borrow().visualizer_presets.list.iter().map(|u| u.name.clone()).collect()
}

/// What settings.json holds now, read back as the app reads it at start.
fn saved(st: &Rc<State>) -> crate::settings::Settings {
    st.settings_dirty.set(true);
    st.flush_settings();
    crate::settings::Settings::load(&st.paths.settings())
}

fn look(st: &State) -> crate::visualizer::look::Look {
    crate::visualizer::settings(st).effective(&ThemeViz::default())
}

/// The pixel at a logical point of a snapshot, as (r, g, b).
fn pixel(img: &image::RgbaImage, app: &AppWindow, x: f32, y: f32) -> [u8; 3] {
    let k = app.window().scale_factor();
    let (px, py) = (((x * k) as u32).min(img.width() - 1), ((y * k) as u32).min(img.height() - 1));
    let p = img.get_pixel(px, py);
    [p[0], p[1], p[2]]
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
    let square = std::env::var("MP_HALO_SHAPE").is_ok_and(|v| v == "square");

    step!(1200, |st, _app, r| {
        size_window(st, 1920, 1080);
        let covers: Vec<(&str, usize)> = ["Night Shift", "Paper", "Graphite"].iter().map(|a| (*a, album(st, a).len())).collect();
        check(r, "fixtures: a dark, a bright and a nearly monochrome cover", covers.iter().all(|c| c.1 >= 1), format!("{covers:?}"));
        crate::explorer::bench_set_clipboard(None);
        playback::play_tracks(st, album(st, "Night Shift"), 0);
        views::go(st, Page::NowPlaying);
    });

    // ---- The halo, in each placement, under three covers, at both sizes ---------------------
    if part("halo") {
        for (w, h, size) in [(1920, 1080, "1080p"), (960, 620, "min")] {
            for (cover, track) in [("Night Shift", 0usize), ("Paper", 0), ("Graphite", 0)] {
                for place in Placement::ALL {
                    step!(150, move |st, app, _r| {
                        size_window(st, w, h);
                        playback::play_tracks(st, album(st, cover), track);
                        playback::seek(st, 7.0);
                        views::go(st, Page::NowPlaying);
                        show(st, app, Preset::Halo, place);
                    });
                    let name = format!("vl-halo{}-{size}-{}-{}", if square { "-square" } else { "" }, place_name(place), cover.to_lowercase().replace(' ', "-"));
                    step!(1700, move |_st, app, r| {
                        let img = snapshot(app, &name);
                        let g = app.global::<Visualizer>();
                        let ok = crate::visualizer::running() == (true, true) && g.get_active() && g.get_ring() && g.get_stage_hub() > 20.0;
                        check(r, &format!("{name}: drawn around the cover"), ok && img.is_some(), state(app));
                        if place == Placement::Behind {
                            // Behind the page the hub is the page's own cover (half its size).
                            check(r, "behind the page: the stage is laid around the page's cover", g.get_stage_width() > g.get_stage_hub() * 2.0 + 60.0, state(app));
                        }
                    });
                }
            }
        }
        // The bonfire, and the other kinds of ring (Saturn, Galaxy, Swarm, Eclipse: the
        // Halo preset, its kind chosen), in each placement, at both sizes.
        let looks: [(&'static str, Preset, Option<usize>); 5] = [
            ("bonfire", Preset::Bonfire, None),
            ("saturn", Preset::Halo, Some(1)),
            ("galaxy", Preset::Halo, Some(2)),
            ("swarm", Preset::Halo, Some(3)),
            ("eclipse", Preset::Halo, Some(4)),
        ];
        for (what, preset, kind) in looks {
            for (w, h, size) in [(1920, 1080, "1080p"), (960, 620, "min")] {
                for place in Placement::ALL {
                    step!(150, move |st, app, _r| {
                        size_window(st, w, h);
                        playback::play_tracks(st, album(st, "Night Shift"), 0);
                        playback::seek(st, 6.0);
                        views::go(st, Page::NowPlaying);
                        show(st, app, preset, place);
                        if let Some(i) = kind {
                            crate::visualizer::choose(st, "ring", i);
                        }
                    });
                    let name = format!("vl-{what}-{size}-{}", place_name(place));
                    step!(2600, move |_st, app, r| {
                        let img = snapshot(app, &name);
                        let g = app.global::<Visualizer>();
                        let paths = crate::visualizer::effect_paths(app);
                        let dots = paths.sparks.matches('M').count() + paths.sparks_old.matches('M').count();
                        let running = crate::visualizer::running() == (true, true) && g.get_active() && img.is_some();
                        let (said, ok) = match what {
                            // Sparks rising from the line along the foot (behind the page, up the whole of it).
                            "bonfire" => ("sparks rising from the line along the foot", g.get_tall() && !g.get_ring() && dots > 5 && !paths.line.is_empty()),
                            // Its dust behind the cover and in front of it, round a round cover.
                            "saturn" => ("dust round the cover, behind it and in front", g.get_ring() && g.get_ring_kind() == 1 && paths.sparks_old.matches('M').count() > 30 && paths.front.matches('M').count() > 30),
                            // The rest: sparks or dust round a round cover, and its thin contour.
                            _ => ("sparks or dust round the cover", g.get_ring() && g.get_ring_kind() == kind.unwrap_or(0) as i32 && dots > 20 && paths.line.starts_with('M') && paths.front.is_empty()),
                        };
                        check(r, &format!("{name}: {said}"), running && ok, format!("{dots} sparks; {}", state(app)));
                    });
                }
            }
        }
        // The cover pulses with the beat (a tiny bit), and not with the switch off.
        step!(150, |st, app, _r| {
            size_window(st, 1920, 1080);
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            playback::seek(st, 3.0);
            show(st, app, Preset::Halo, Placement::Page);
        });
        let largest = Rc::new(Cell::new(1.0f32));
        for _ in 0..40 {
            let largest = largest.clone();
            step!(50, move |_st, app, _r| largest.set(largest.get().max(crate::visualizer::presented_cover_scale(app))));
        }
        {
            let largest = largest.clone();
            step!(50, move |st, app, r| {
                check(r, "the ring's cover grows a tiny bit with a beat", largest.get() > 1.008 && largest.get() <= 1.0251, format!("{:.4}", largest.get()));
                check(r, "Settings says so", app.global::<Visualizer>().get_cover_pulse() && app.global::<Visualizer>().get_ring_kind() == 0, String::new());
                crate::visualizer::toggle(st, "cover_pulse");
                largest.set(1.0);
            });
        }
        step!(300, |_st, _app, _r| {});
        for _ in 0..30 {
            let largest = largest.clone();
            step!(50, move |_st, app, _r| largest.set(largest.get().max(crate::visualizer::presented_cover_scale(app))));
        }
        {
            let largest = largest.clone();
            step!(50, move |st, app, r| {
                check(r, "switched off: the cover keeps its size", largest.get() == 1.0 && !app.global::<Visualizer>().get_cover_pulse(), format!("{:.4}", largest.get()));
                crate::visualizer::toggle(st, "cover_pulse");
            });
        }
        // The halo moves: two frames of the same moment differ, and are not the old ring's
        // (its edge is one closed curve, its band a filled shape with a hole).
        step!(150, |st, app, _r| {
            size_window(st, 1920, 1080);
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            playback::seek(st, 5.0);
            show(st, app, Preset::Halo, Placement::Page);
        });
        let first = Rc::new(RefCell::new(String::new()));
        {
            let first = first.clone();
            step!(1200, move |_st, app, _r| *first.borrow_mut() = crate::visualizer::effect_paths(app).line);
        }
        step!(400, move |_st, app, r| {
            let g = app.global::<Visualizer>();
            let paths = crate::visualizer::effect_paths(app);
            let edge = paths.line;
            check(r, "the halo's edge moves with the music, one closed curve", edge != *first.borrow() && edge.starts_with('M') && edge.ends_with('Z') && edge.matches('M').count() == 1, format!("{} bytes", edge.len()));
            let (fill, ripples) = (paths.fill, paths.ripples);
            check(r, "its corona: the contour with the cover's place left out (two loops, the second the foot's circle)", fill.matches('M').count() == 2 && fill.matches('Z').count() == 2 && fill.contains('A'), format!("{} loops", fill.matches('M').count()));
            check(r, "its glow: a wide faint stroke under the edge", g.get_glow_width() >= 3.0, format!("glow {}", g.get_glow_width()));
            // 1.6 s of music: a ripple has left by the beat or by the rest, and none more than the most at once.
            let n = ripples.matches('M').count();
            check(r, "its ripples: closed curves on their way out", (1..=6).contains(&n) && n == ripples.matches('Z').count() && g.get_ripples_width() >= 1.0, format!("{n} ripples, {} px", g.get_ripples_width()));
            let dots = paths.sparks.matches('M').count() + paths.sparks_old.matches('M').count();
            check(r, "its sparks: thrown off the contour, no more than the most at once", (1..=240).contains(&dots) && !paths.sparks_glow.is_empty(), format!("{dots} sparks"));
        });
        // The beat is heard in real tracks: the house fixture has a kick on every beat at 124
        // a minute under an off-beat bass line, the hip-hop one a long 808 at 88.
        for (track, name, bpm) in [(0usize, "house", 124.0f32), (1, "hip-hop", 88.0)] {
            let before = Rc::new(Cell::new(0u32));
            {
                let before = before.clone();
                step!(150, move |st, app, _r| {
                    playback::play_tracks(st, album(st, "Night Shift"), track);
                    playback::seek(st, 2.0);
                    show(st, app, Preset::Halo, Placement::Page);
                    before.set(crate::visualizer::heard().1);
                });
            }
            step!(8000, move |_st, _app, r| {
                let (got, hits) = crate::visualizer::heard();
                // Eight seconds of it: about as many beats shown as were played.
                let (shown, played) = ((hits - before.get()) as f32, 8.0 * bpm / 60.0);
                check(r, &format!("{name}: the tempo is heard, {bpm} beats a minute"), (got - bpm).abs() < 3.0, format!("{got:.1}"));
                // (The hip-hop bar has two kicks to its four beats: a beat without one is shown
                // only while the tempo fits well, so anything from half of them up.)
                check(r, &format!("{name}: a beat shown for each one played, near enough"), shown >= played * 0.5 && shown < played * 1.4, format!("{shown} shown, {played:.0} played"));
            });
        }
    }

    // ---- The pinned preview -------------------------------------------------------------------
    if part("pin") {
        step!(200, |st, app, _r| {
            size_window(st, 1920, 1080);
            show(st, app, Preset::Classic, Placement::Behind);
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            playback::seek(st, 6.0);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        step!(1300, |_st, app, r| {
            let v = app.global::<SettingsView>();
            snapshot(app, "vl-pin-wide-top");
            check(r, "wide: the preview beside the column, live", v.get_pin_side() && crate::visualizer::running().0 && !app.global::<Visualizer>().get_still(), state(app));
            let g = app.global::<Visualizer>();
            check(r, "drawn once: one stage, the side card's", g.get_stage_width() > 300.0 && g.get_stage_width() < 560.0, state(app));
            app.global::<Nav>().set_page_scroll(-900.0);
        });
        step!(700, |_st, app, r| {
            let img = snapshot(app, "vl-pin-wide-scrolled");
            check(r, "scrolled: the preview stays where it was, still running", crate::visualizer::running().0 && app.global::<Visualizer>().get_active(), state(app));
            let _ = img;
            app.global::<Nav>().set_page_scroll(-5000.0);
        });
        shot!(600, "vl-pin-wide-bottom".into());
        // Narrow: held at the column's top once the column scrolls past it.
        step!(200, |st, app, _r| {
            size_window(st, 960, 620);
            app.global::<Nav>().set_page_scroll(0.0);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        step!(1300, |_st, app, r| {
            let v = app.global::<SettingsView>();
            snapshot(app, "vl-pin-min-top");
            check(r, "narrow: the preview in the column, compact, live", !v.get_pin_side() && v.get_pin_height() > 100.0 && v.get_pin_height() < 160.0 && crate::visualizer::running().0, format!("height {}; {}", v.get_pin_height(), state(app)));
            app.global::<Nav>().set_page_scroll(-400.0);
        });
        step!(700, |_st, app, r| {
            let img = snapshot(app, "vl-pin-min-scrolled");
            let v = app.global::<SettingsView>();
            // The card at the column's top: its border's colour there, not the column's text.
            let held = img.as_ref().map(|i| pixel(i, app, 256.0 + 60.0, v.get_column_top() + 6.0));
            check(r, "scrolled: the preview held at the column's top, the column cut under it", v.get_pin_cover() > 100.0 && crate::visualizer::running().0, format!("cover {} px, pixel {held:?}", v.get_pin_cover()));
            app.global::<Nav>().set_page_scroll(-3000.0);
        });
        shot!(600, "vl-pin-min-bottom".into());
        // Tab through the section with the preview held: every control the Tab key reaches is
        // shown below it.
        step!(200, |st, app, _r| {
            app.global::<Nav>().set_page_scroll(0.0);
            views::settings_section(st, settings::nav::VISUALIZER);
            app.invoke_focus_shortcuts();
        });
        step!(300, |_st, app, r| {
            // The filter box, the list, the switch, Where, Opacity, the presets, Presets, Reset,
            // Style, Colours, Detail...
            tabs(app, 11);
            let v = app.global::<SettingsView>();
            check(r, "Tab moves down the column; the preview stays held", app.global::<Nav>().get_page_scroll() < 0.0 && v.get_pin_cover() > 0.0, format!("scroll {}", app.global::<Nav>().get_page_scroll()));
        });
        shot!(400, "vl-pin-min-tab".into());
        // Nothing playing: the still frame, nothing running.
        step!(200, |st, _app, _r| playback::set_paused(st, true));
        step!(2600, |_st, app, r| {
            snapshot(app, "vl-pin-min-still");
            check(r, "paused: the preview's still frame, nothing running", !crate::visualizer::running().0 && app.global::<Visualizer>().get_still() && app.global::<Visualizer>().get_active(), state(app));
        });
        // The filter's results replace the section: no preview, nothing drawn.
        step!(200, |st, _app, _r| {
            playback::set_paused(st, false);
            views::settings_filter(st, "opacity");
        });
        step!(700, |_st, app, r| {
            snapshot(app, "vl-pin-filter");
            let v = app.global::<SettingsView>();
            check(r, "Find a setting: its results, no preview, nothing running", v.get_hits().row_count() >= 1 && !crate::visualizer::running().0 && !app.global::<Visualizer>().get_active(), format!("{} hits; {}", v.get_hits().row_count(), state(app)));
            let hits: Vec<String> = settings::nav::find("opacity").into_iter().map(|h| h.label).collect();
            check(r, "it finds Opacity", hits.iter().any(|h| h == "Opacity"), format!("{hits:?}"));
            let hits: Vec<String> = settings::nav::find("share").into_iter().map(|h| h.label).collect();
            check(r, "and the user's presets by what they are about", hits.iter().any(|h| h == "Your presets"), format!("{hits:?}"));
        });
        step!(200, |st, _app, _r| views::settings_section(st, settings::nav::VISUALIZER));
        step!(900, |_st, app, r| check(r, "the section again: the preview runs", crate::visualizer::running().0, state(app)));
        // Wide again; the scroll on entering the section is the top.
        step!(200, |st, app, _r| {
            size_window(st, 1920, 1080);
            app.global::<Nav>().set_page_scroll(-700.0);
            views::settings_section(st, settings::nav::EQUALIZER);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        step!(900, |_st, app, r| check(r, "entering the section: at its top", app.global::<Nav>().get_page_scroll() == 0.0, format!("{}", app.global::<Nav>().get_page_scroll())));
        step!(200, |st, app, _r| {
            show(st, app, Preset::Halo, Placement::Behind);
        });
        shot!(1200, "vl-pin-wide-halo".into());
        step!(200, |st, _app, _r| {
            size_window(st, 960, 620);
            views::settings_section(st, settings::nav::VISUALIZER);
        });
        shot!(1200, "vl-pin-min-halo".into());
        // The equalizer's curve, pinned the same way: beside the column, then held at the top
        // of a narrow one while its bands scroll under it.
        step!(200, |st, app, _r| {
            size_window(st, 1920, 1080);
            crate::eq::choose(st, "Bass boost");
            crate::eq::set_mode(st, true);
            app.global::<Nav>().set_page_scroll(0.0);
            views::settings_section(st, settings::nav::EQUALIZER);
        });
        step!(900, |_st, app, r| {
            snapshot(app, "vl-pin-eq-wide");
            check(r, "the equalizer: its curve beside the column", app.global::<SettingsView>().get_pin_side() && !app.global::<crate::Equalizer>().get_curve().is_empty(), String::new());
            app.global::<Nav>().set_page_scroll(-600.0);
        });
        shot!(600, "vl-pin-eq-wide-scrolled".into());
        step!(200, |st, app, _r| {
            size_window(st, 960, 620);
            app.global::<Nav>().set_page_scroll(0.0);
            views::settings_section(st, settings::nav::EQUALIZER);
        });
        shot!(900, "vl-pin-eq-min-top".into());
        step!(200, |_st, app, _r| app.global::<Nav>().set_page_scroll(-500.0));
        step!(700, |st, app, r| {
            snapshot(app, "vl-pin-eq-min-scrolled");
            let v = app.global::<SettingsView>();
            check(r, "narrow: the curve held at the column's top, the bands scrolling under it", !v.get_pin_side() && v.get_pin_cover() > 100.0 && app.global::<Nav>().get_page_scroll() < 0.0, format!("cover {}", v.get_pin_cover()));
            crate::eq::reset(st);
            if st.settings.borrow().equalizer.all.enabled {
                crate::eq::toggle(st);
            }
        });
        step!(200, |st, _app, _r| size_window(st, 1920, 1080));
    }

    // ---- The placement key ------------------------------------------------------------------
    if part("keys") {
        step!(200, |st, app, _r| {
            show(st, app, Preset::Classic, Placement::Behind);
            views::go(st, Page::NowPlaying);
            app.invoke_focus_shortcuts();
        });
        step!(500, |st, app, r| {
            chord(app, Key::Shift, "V");
            let s = crate::visualizer::settings(st);
            check(r, "Shift+V: in the panel, its tab chosen, said", s.placement == Placement::Panel && app.global::<Visualizer>().get_tab() && flash(app) == "Visualizer: in the panel", format!("{:?}, {:?}", s.placement, flash(app)));
        });
        step!(700, |st, app, r| {
            snapshot(app, "vl-keys-panel");
            check(r, "running in the panel", crate::visualizer::running().0, state(app));
            chord(app, Key::Shift, "V");
            check(r, "Shift+V: full page", crate::visualizer::settings(st).placement == Placement::Page && flash(app) == "Visualizer: full page", flash(app));
        });
        step!(500, |st, app, r| {
            chord(app, Key::Shift, "V");
            check(r, "Shift+V: behind the page, and round", crate::visualizer::settings(st).placement == Placement::Behind && flash(app) == "Visualizer: behind the page", flash(app));
            // V alone: off.
            type_text(app, "v");
            check(r, "V alone still turns it off", !st.settings.borrow().visualizer, String::new());
        });
        step!(600, |st, app, r| {
            check(r, "off: nothing runs", crate::visualizer::running() == (false, false), state(app));
            chord(app, Key::Shift, "V");
            check(r, "off, Shift+V: on, where it was (nothing moves)", st.settings.borrow().visualizer && crate::visualizer::settings(st).placement == Placement::Behind && flash(app) == "Visualizer: on, behind the page", flash(app));
        });
        step!(600, |st, app, r| {
            check(r, "and running", crate::visualizer::running().0, state(app));
            views::go(st, Page::Albums);
            app.invoke_focus_shortcuts();
        });
        step!(500, |st, app, r| {
            chord(app, Key::Shift, "V");
            check(r, "on another page: it moves, and the pill says where it shows", crate::visualizer::settings(st).placement == Placement::Panel && flash(app) == "Visualizer: in the panel (on Now playing)" && crate::visualizer::running() == (false, false), format!("{:?}; {}", flash(app), state(app)));
        });
        shot!(400, "vl-keys-other-page".into());
        step!(300, |st, app, r| {
            views::settings_section(st, settings::nav::KEYS);
            let rows = app.global::<Keys>().get_rows();
            let row = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).find(|k| k.what.contains("Where the visualizer shows"));
            check(r, "Settings > Keys lists it, Shift+V", row.as_ref().is_some_and(|k| k.key0 == "Shift+V"), format!("{:?}", row.map(|k| (k.what, k.key0))));
            let hits: Vec<String> = settings::nav::find("where the visualizer shows").into_iter().map(|h| h.label).collect();
            check(r, "and Find a setting finds it", hits.iter().any(|h| h.starts_with("Where the visualizer shows")), format!("{hits:?}"));
        });
        shot!(500, "vl-keys-list".into());
        // Remapped in the editor (the capture by Slint's dispatch): Shift+P, then back.
        step!(300, |st, app, r| {
            let row = crate::keys::map::Action::ALL.iter().position(|a| *a == crate::keys::map::Action::VisualizerPlace).unwrap_or(0) as i32;
            crate::keys::edit::capture(st, row, 0);
            crate::keys::edit::captured(st, "P", false, true, false);
            check(r, "the editor: Shift+P", st.keymap.borrow().label(crate::keys::map::Action::VisualizerPlace) == "Shift+P" && app.global::<Keys>().get_visualizer_place() == "Shift+P", st.keymap.borrow().label(crate::keys::map::Action::VisualizerPlace));
            check(r, "saved as a change from the default", st.settings.borrow().shortcuts.get("visualizer_placement").is_some_and(|v| v == &vec!["Shift+P".to_string()]), format!("{:?}", st.settings.borrow().shortcuts));
            st.keymap.borrow_mut().reset(crate::keys::map::Action::VisualizerPlace);
            st.settings.borrow_mut().shortcuts = st.keymap.borrow().overrides();
            crate::keys::edit::refresh(st);
            check(r, "reset: Shift+V again", st.keymap.borrow().label(crate::keys::map::Action::VisualizerPlace) == "Shift+V", String::new());
            show(st, app, Preset::Classic, Placement::Behind);
        });
    }

    // ---- Opacity ----------------------------------------------------------------------------
    if part("opacity") {
        step!(200, |st, app, _r| {
            size_window(st, 1920, 1080);
            show(st, app, Preset::Prism, Placement::Page);
            views::settings_section(st, settings::nav::VISUALIZER);
            app.global::<Nav>().set_page_scroll(0.0);
            app.invoke_focus_shortcuts();
        });
        step!(600, |st, app, r| {
            let before = look(st);
            // The filter box, the list, the switch, Where, Opacity; Left: 5% down.
            tabs(app, 5);
            press(app, Key::LeftArrow, &[]);
            press(app, Key::LeftArrow, &[]);
            let after = look(st);
            check(r, "Opacity by the keyboard, on the full page: 100% to 90%", (before.opacity - 1.0).abs() < 1e-4 && (after.opacity - 0.9).abs() < 1e-4 && (after.prominence - before.prominence).abs() < 1e-4 && app.global::<Visualizer>().get_opacity() == "90%", format!("{} -> {}, behind {}", before.opacity, after.opacity, after.prominence));
        });
        step!(300, |st, app, r| {
            app.global::<crate::Actions>().invoke_viz_step("opacity".into(), -4);
            check(r, "and by the pointer: 70%", (look(st).opacity - 0.7).abs() < 1e-4, format!("{}", look(st).opacity));
            check(r, "a look of its own: Custom", crate::visualizer::settings(st).preset == "custom", crate::visualizer::settings(st).preset.clone());
            views::go(st, Page::NowPlaying);
        });
        shot!(1300, "vl-opacity-page-70".into());
        // The quick menu (its button), then the keyboard: Down to Opacity, Right, Down to 40%,
        // Enter.
        step!(200, |st, _app, _r| crate::visualizer::menu(st, 1600.0, 80.0, false));
        step!(400, |_st, app, r| {
            let l = menu_labels(app);
            check(r, "the quick menu: Opacity, as it is here", l.iter().any(|x| x == "Opacity: 70%"), format!("{l:?}"));
            let at = l.iter().position(|x| x.starts_with("Opacity")).unwrap_or(0);
            for _ in 0..=at {
                press(app, Key::DownArrow, &[]);
            }
            press(app, Key::RightArrow, &[]);
        });
        step!(400, |_st, app, r| {
            snapshot(app, "vl-opacity-menu");
            let m = app.global::<MenuStore>();
            let sub: Vec<String> = (0..m.get_sub().row_count()).filter_map(|i| m.get_sub().row_data(i)).map(|x| x.label.to_string()).collect();
            check(r, "Right opens its steps, 70% checked", sub.len() == 10 && m.get_sub().row_data(3).is_some_and(|x| x.checked && x.label == "70%"), format!("{sub:?}"));
            // From 100% (the first) down to 40%.
            for _ in 0..6 {
                press(app, Key::DownArrow, &[]);
            }
            press(app, Key::Return, &[]);
        });
        step!(1200, |st, app, r| {
            snapshot(app, "vl-opacity-page-40");
            check(r, "40% from the menu, said", (look(st).opacity - 0.4).abs() < 1e-4 && flash(app) == "Visualizer opacity 40%", format!("{} {:?}", look(st).opacity, flash(app)));
            // Behind the page it has its own.
            crate::visualizer::choose(st, "placement", 0);
            check(r, "behind the page: its own opacity (Prism's 85%)", app.global::<Visualizer>().get_opacity() == "85%" && (look(st).prominence - 0.85).abs() < 1e-4, app.global::<Visualizer>().get_opacity().to_string());
            crate::visualizer::step(st, "opacity", -9);
            check(r, "lowered there, the full page's stays", (look(st).prominence - 0.4).abs() < 1e-4 && (look(st).opacity - 0.4).abs() < 1e-4, format!("{} {}", look(st).prominence, look(st).opacity));
        });
        shot!(1300, "vl-opacity-behind-40".into());
        step!(200, |st, _app, r| {
            let s = saved(st);
            let l = s.visualizer_look.as_ref().map(|v| v.effective(&ThemeViz::default()));
            check(r, "saved: both opacities", l.as_ref().is_some_and(|l| (l.opacity - 0.4).abs() < 1e-4 && (l.prominence - 0.4).abs() < 1e-4), format!("{:?}", l.map(|l| (l.opacity, l.prominence))));
        });
    }

    // ---- The user's presets -----------------------------------------------------------------
    if part("presets") {
        step!(200, |st, app, _r| {
            size_window(st, 1920, 1080);
            playback::play_tracks(st, album(st, "Night Shift"), 0);
            show(st, app, Preset::Halo, Placement::Page);
            crate::visualizer::choose(st, "colours", 7);
            crate::visualizer::step(st, "sensitivity", 1);
            views::settings_section(st, settings::nav::VISUALIZER);
            app.global::<Nav>().set_page_scroll(0.0);
            app.invoke_focus_shortcuts();
        });
        // Save as, by the keyboard: Tab to Presets, Enter, Enter on "Save as preset…", the
        // name typed over the one offered, Enter.
        step!(600, |_st, app, _r| {
            // The filter box, the list, the switch, Where, Opacity, the presets, Presets.
            tabs(app, 7);
            press(app, Key::Return, &[]);
        });
        step!(400, |_st, app, r| {
            let l = menu_labels(app);
            check(r, "the presets menu", l.first().map(String::as_str) == Some("Save as preset\u{2026}") && l.iter().any(|x| x == "Paste a preset\u{2026}") && l.iter().any(|x| x == "Import a file\u{2026}"), format!("{l:?}"));
            snapshot(app, "vl-presets-menu");
            press(app, Key::DownArrow, &[]);
            press(app, Key::Return, &[]);
        });
        step!(400, |_st, app, r| {
            let d = app.global::<Dialog>();
            check(r, "Save as: the name prompt", d.get_prompt_open() && d.get_prompt_title().contains("preset"), d.get_prompt_title().to_string());
            press(app, Key::Home, &[Key::Shift]);
            press(app, Key::End, &[Key::Shift]);
            type_text(app, "Night halo");
        });
        shot!(300, "vl-presets-save-prompt".into());
        step!(300, |st, app, r| {
            press(app, Key::Return, &[]);
            check(r, "saved by the keyboard, and in use", users(st) == ["Night halo"] && app.global::<Visualizer>().get_user_current() == "Night halo" && !app.global::<Dialog>().get_prompt_open(), format!("{:?} {:?}", users(st), app.global::<Visualizer>().get_user_current()));
            let p = app.global::<Visualizer>().get_presets();
            check(r, "the picker lists it after the built-in ones, chosen", p.row_count() == Preset::ALL.len() + 1 && p.row_data(Preset::ALL.len()).is_some_and(|x| x.user && x.name == "Night halo") && app.global::<Visualizer>().get_preset() == Preset::ALL.len() as i32, format!("{} rows, preset {}", p.row_count(), app.global::<Visualizer>().get_preset()));
            let s = saved(st);
            check(r, "settings.json has it", s.visualizer_presets.list.iter().any(|u| u.name == "Night halo" && u.look.style == Style::Radial), format!("{:?}", s.visualizer_presets.list.iter().map(|u| &u.name).collect::<Vec<_>>()));
        });
        shot!(600, "vl-presets-saved".into());
        // The look key: Classic... then into the user's, then round.
        step!(200, |st, app, r| {
            crate::visualizer::choose_preset(st, Preset::ALL.len() - 1);
            app.invoke_focus_shortcuts();
            type_text(app, "c");
            check(r, "C after the last built-in one: the user's, said", app.global::<Visualizer>().get_user_current() == "Night halo" && flash(app) == "Visualizer: Night halo", flash(app));
            type_text(app, "c");
            check(r, "C again: Classic", crate::visualizer::settings(st).preset == "classic", crate::visualizer::settings(st).preset.clone());
            crate::visualizer::choose_preset(st, Preset::ALL.len());
        });
        // Rename by the pointer: the menu at the button, its entry clicked, a name typed.
        step!(300, |st, app, r| {
            crate::visualizer::presets::open_menu(st, 900.0, 400.0);
            let l = menu_labels(app);
            check(r, "for one of the user's: Rename and Delete (no Update: unchanged)", l.iter().any(|x| x == "Rename \u{201c}Night halo\u{201d}\u{2026}") && l.iter().any(|x| x == "Delete \u{201c}Night halo\u{201d}") && !l.iter().any(|x| x.starts_with("Update")), format!("{l:?}"));
            let i = l.iter().position(|x| x.starts_with("Rename")).unwrap_or(0) as i32;
            app.global::<crate::Actions>().invoke_menu_activate(i, -1);
        });
        step!(300, |st, app, r| {
            app.global::<crate::Actions>().invoke_prompt_confirm("Halo at night".into());
            check(r, "renamed, still in use", users(st) == ["Halo at night"] && app.global::<Visualizer>().get_user_current() == "Halo at night", format!("{:?}", users(st)));
            // A built-in name is refused.
            crate::visualizer::presets::save_prompt(st);
            app.global::<crate::Actions>().invoke_prompt_confirm("prism".into());
            check(r, "a built-in name is refused in the prompt", app.global::<Dialog>().get_prompt_open() && app.global::<Dialog>().get_prompt_note().contains("built-in"), app.global::<Dialog>().get_prompt_note().to_string());
            app.global::<crate::Actions>().invoke_prompt_cancel();
        });
        // Update: change it, Update saves the change to it.
        step!(300, |st, app, r| {
            crate::visualizer::choose(st, "motion", 0);
            check(r, "changed: Custom, from it", app.global::<Visualizer>().get_user_based() == "Halo at night" && app.global::<Visualizer>().get_preset() == -1, app.global::<Visualizer>().get_user_based().to_string());
            let l: Vec<String> = crate::visualizer::presets::menu_entries(st).iter().map(|e| format!("{e:?}")).collect();
            check(r, "the menu offers Update", l.iter().any(|x| x.contains("Update \u{201c}Halo at night\u{201d} to this look")), format!("{l:?}"));
            crate::visualizer::presets::update(st);
            let s = saved(st);
            let u = s.visualizer_presets.list.first();
            check(r, "updated: in use again, saved with the change", app.global::<Visualizer>().get_user_current() == "Halo at night" && u.is_some_and(|u| u.look.motion == crate::visualizer::look::Motion::Smooth), format!("{:?}", u.map(|u| u.look.motion)));
        });
        // Copy: one line on the (stand-in) clipboard, by the menu and by Ctrl+C on the page.
        step!(300, |st, app, r| {
            crate::explorer::bench_set_clipboard(None);
            crate::visualizer::presets::open_menu(st, 900.0, 400.0);
            let l = menu_labels(app);
            let i = l.iter().position(|x| x.starts_with("Copy")).unwrap_or(0) as i32;
            app.global::<crate::Actions>().invoke_menu_activate(i, -1);
            let text = clip();
            check(r, "Copy: one line of text, its name and look", text.starts_with("MarqueeViz/1 name=\"Halo at night\" style=ring") && !text.contains('\n'), text.clone());
            line(r, format!("  the text: {text}"));
            check(r, "the pill says so", flash(app).starts_with("Visualizer look copied"), flash(app));
            crate::explorer::bench_set_clipboard(None);
            app.invoke_focus_shortcuts();
            chord(app, Key::Control, "c");
            check(r, "Ctrl+C on the page copies it too", clip().starts_with("MarqueeViz/1 name=\"Halo at night\""), clip());
        });
        // Paste the same: it is that preset already ("Use it").
        step!(300, |st, _app, _r| crate::visualizer::presets::paste(st));
        step!(500, |_st, app, r| {
            let g = app.global::<Visualizer>();
            check(r, "pasted the same: shown, \u{201c}you have this preset already\u{201d}", g.get_trial() && g.get_trial_identical() && g.get_trial_text().contains("already"), g.get_trial_text().to_string());
            snapshot(app, "vl-presets-paste-same");
            press(app, Key::Escape, &[]);
            check(r, "Esc: gone, nothing added", !g.get_trial(), String::new());
        });
        // Paste a friend's preset by Ctrl+V: shown in the preview first, added by Enter.
        step!(300, |_st, app, _r| {
            crate::explorer::bench_set_clipboard(Some("Here's mine! MarqueeViz/1 name=\"Ember ring\" style=ring colours=ember by=frequency detail=fine motion=normal sensitivity=+3 peaks=on opacity=90% behind=70%  (looks great with jazz)"));
            app.invoke_focus_shortcuts();
            chord(app, Key::Control, "v");
        });
        step!(900, |st, app, r| {
            let g = app.global::<Visualizer>();
            let t = crate::visualizer::presets::trial();
            check(r, "Ctrl+V: the friend's preset shown in the preview, not added yet", g.get_trial() && g.get_trial_name() == "Ember ring" && users(st) == ["Halo at night"] && t.as_ref().is_some_and(|t| t.look.colours == crate::visualizer::look::Colours::Ember), g.get_trial_text().to_string());
            check(r, "the preview draws it (the settings below are still the look in use)", g.get_ring() && crate::visualizer::running().0 && app.global::<Visualizer>().get_colours() == 7, state(app));
            snapshot(app, "vl-presets-paste-friend");
            press(app, Key::Return, &[]);
            check(r, "Enter: added and in use", users(st) == ["Halo at night", "Ember ring"] && g.get_user_current() == "Ember ring" && !g.get_trial(), format!("{:?}", users(st)));
        });
        shot!(700, "vl-presets-added".into());
        // The same name again, another look: Replace or Keep both; Keep both by the pointer.
        step!(300, |st, _app, _r| {
            crate::explorer::bench_set_clipboard(Some("MarqueeViz/1 name=\"Ember ring\" style=bars colours=ember"));
            crate::visualizer::presets::paste(st);
        });
        step!(600, |st, app, r| {
            let g = app.global::<Visualizer>();
            check(r, "a name you have: Replace or Keep both", g.get_trial() && g.get_trial_taken() && !g.get_trial_identical(), g.get_trial_text().to_string());
            snapshot(app, "vl-presets-paste-taken");
            app.global::<crate::Actions>().invoke_viz_trial("keep".into());
            check(r, "Keep both: added beside it", users(st) == ["Halo at night", "Ember ring", "Ember ring (2)"] && g.get_user_current() == "Ember ring (2)", format!("{:?}", users(st)));
            crate::explorer::bench_set_clipboard(Some("MarqueeViz/1 name=\"Ember ring\" style=line colours=ember"));
            crate::visualizer::presets::paste(st);
            app.global::<crate::Actions>().invoke_viz_trial("replace".into());
            let s = saved(st);
            check(r, "Replace: the one of that name takes it", users(st).len() == 3 && s.visualizer_presets.list.iter().find(|u| u.name == "Ember ring").is_some_and(|u| u.look.style == Style::Line), format!("{:?}", users(st)));
        });
        // Refused pastes: said, nothing shown, nothing changed.
        let refused: [(&'static str, &'static str, String); 4] = [
            ("not a preset", "Nothing to paste: no visualizer preset on the clipboard", "Shopping: milk, eggs".to_owned()),
            ("a newer format", "newer version of Marquee Player (format 2)", "MarqueeViz/2 name=\"Future\" style=hologram".to_owned()),
            ("an unknown style", "the style \u{201c}plasma\u{201d}", "MarqueeViz/1 style=plasma".to_owned()),
            ("far too long", "too long", format!("MarqueeViz/1 name=\"{}\"", "x".repeat(40_000))),
        ];
        for (what, says, text) in refused {
            step!(300, move |st, app, r| {
                let before = users(st);
                crate::explorer::bench_set_clipboard(Some(&text));
                crate::visualizer::presets::paste(st);
                check(r, &format!("refused: {what}, said"), flash(app).contains(says) && !app.global::<Visualizer>().get_trial() && users(st) == before, flash(app));
            });
        }
        shot!(400, "vl-presets-refused".into());
        // Export, then import the file: shown, cancelled by Esc; again, added by the pointer.
        step!(300, |st, app, r| {
            crate::visualizer::choose_preset(st, Preset::ALL.len());
            let path = out_dir().join(crate::visualizer::presets::file_name("Halo at night"));
            let ok = crate::visualizer::presets::export_file(st, &path);
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            check(r, "Export: a file of the line", ok && text.lines().any(|l| l.starts_with("MarqueeViz/1 name=\"Halo at night\"")), text.clone());
            crate::visualizer::presets::delete(st);
            check(r, "Delete: gone, the look stays as Custom", users(st) == ["Ember ring", "Ember ring (2)"] && app.global::<Visualizer>().get_preset() == -1 && saved(st).visualizer_presets.list.len() == 2, format!("{:?}", users(st)));
            let res = crate::visualizer::presets::import_file(st, &path);
            check(r, "Import: read", res.is_ok(), format!("{res:?}"));
        });
        step!(600, |st, app, r| {
            let g = app.global::<Visualizer>();
            check(r, "the file's preset shown, named, from the file", g.get_trial() && g.get_trial_name() == "Halo at night" && g.get_trial_text().contains(".mpviz"), g.get_trial_text().to_string());
            snapshot(app, "vl-presets-import");
            press(app, Key::Escape, &[]);
            check(r, "Esc: nothing added", !g.get_trial() && users(st).len() == 2, format!("{:?}", users(st)));
            let path = out_dir().join(crate::visualizer::presets::file_name("Halo at night"));
            let _ = crate::visualizer::presets::import_file(st, &path);
        });
        step!(600, |st, app, r| {
            app.global::<crate::Actions>().invoke_viz_trial("add".into());
            check(r, "imported again and added by the pointer", users(st) == ["Ember ring", "Ember ring (2)", "Halo at night"] && app.global::<Visualizer>().get_user_current() == "Halo at night", format!("{:?}", users(st)));
            let bad = out_dir().join("not-a-preset.mpviz");
            let _ = std::fs::write(&bad, [0u8, 159, 146, 150, 0, 1, 2]);
            let _ = crate::visualizer::presets::import_file(st, &bad);
            check(r, "a binary file is refused, said", flash(app).contains("not-a-preset.mpviz") && !app.global::<Visualizer>().get_trial(), flash(app));
        });
        // The quick menu lists them; one chosen there.
        step!(300, |st, app, _r| {
            views::go(st, Page::NowPlaying);
            crate::visualizer::menu(st, 1600.0, 80.0, false);
            let l = menu_labels(app);
            let i = l.iter().position(|x| x.starts_with("Your presets")).unwrap_or(0) as i32;
            app.global::<crate::Actions>().invoke_menu_sub(i);
        });
        step!(400, |st, app, r| {
            snapshot(app, "vl-presets-quick-menu");
            let m = app.global::<MenuStore>();
            let sub: Vec<String> = (0..m.get_sub().row_count()).filter_map(|i| m.get_sub().row_data(i)).map(|x| x.label.to_string()).collect();
            check(r, "the quick menu: the user's, in their order, the one in use checked", sub == ["Ember ring", "Ember ring (2)", "Halo at night"] && m.get_sub().row_data(2).is_some_and(|x| x.checked), format!("{sub:?}"));
            app.global::<crate::Actions>().invoke_menu_activate(m.get_sub_of(), 0);
            check(r, "chosen there", app.global::<Visualizer>().get_user_current() == "Ember ring", app.global::<Visualizer>().get_user_current().to_string());
            let _ = st;
        });
        shot!(1300, "vl-presets-ember-page".into());
        // What this build wrote loads in this build.
        step!(300, |st, _app, r| {
            let s = saved(st);
            let names: Vec<String> = s.visualizer_presets.list.iter().map(|u| u.name.clone()).collect();
            check(r, "settings.json loads back with every preset", names == ["Ember ring", "Ember ring (2)", "Halo at night"] && s.visualizer_presets.kept.is_empty(), format!("{names:?}"));
        });
    }

    step!(300, |st, _app, r| {
        crate::explorer::bench_set_clipboard(None);
        let _ = st;
        line(r, format!("failed checks: {}", FAILS.with(Cell::get)));
        write_report("viz-look", &r.borrow());
        let _ = slint::quit_event_loop();
    });
    run_steps(steps);
}
