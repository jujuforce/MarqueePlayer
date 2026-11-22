//! The visualizer: when it runs, what it reads, and handing each frame to the UI. Its look
//! is `visualizer/look.rs` (the choices, the presets, settings.json), its drawing `visualizer/draw.rs`.
//!
//! It draws on one stage at a time (`VisualizerStage` in vizstage.slint): the band behind Now
//! playing, the panel's Visualizer tab, the full page, or the preview in Settings >
//! Visualizer. It runs only while that stage is on screen (the visualizer on, Now playing
//! shown with that placement, or the Settings section shown), the window is shown, not
//! minimized and not covered (`ticker::shown`: behind other windows or on another Space on
//! macOS), and music plays: then a timer at the display's refresh rate (at most 60 a
//! second, or 30 when asked) reads the engine's tap (`player_core::spectrum::Tap`: the
//! samples being heard, after ReplayGain, the equalizer and the limiter, before volume),
//! analyses them (`Analyzer`, the spectrum) and draws. Paused
//! or stopped, it falls to rest, and then the timer, the tap and the analysis all stop;
//! leaving the page or section, hiding, minimizing or covering the window stops them at once. With
//! nothing playing, the Settings preview shows one still frame of made-up music, drawn
//! once. Nothing else wakes for it.
//!
//! Drawing: a few `Path`s (the bars or area, the caps, the line and its glow, the
//! reflection), each one string of commands rebuilt per frame, with brushes built here
//! when the look, the stage or the colours change (not per frame).
//!
//! `look` is its looks, presets and settings, `draw` each style's geometry and brushes,
//! `presets` the user's presets, `share` a preset as one line of text, `beat` the beat
//! detection.

pub(crate) mod beat;
pub(crate) mod draw;
pub(crate) mod look;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
pub(crate) mod presets;
pub(crate) mod share;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::PlayState;
use player_core::spectrum::{Analyzer, Config, Tap};
use slint::winit_030::WinitWindowAccessor;
use slint::{Brush, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::state::{self, State};
use crate::visualizer::draw::{Brushes, Draw, Stage};
use crate::visualizer::look::{ColourBy, Colours, Detail, Fps, Look, Motion, Pick, Placement, Preset, Style, ThemeViz, VizSettings};
use crate::{AppWindow, Nav, Page, SettingsView, Theme, Visualizer};

/// `MP_VIZ_DRAW` (measurement only): "none" analyses without drawing, "minimal" redraws
/// a 1 px rectangle per frame; anything else (the default) draws.
#[derive(Clone, Copy, PartialEq)]
enum DrawMode {
    Full,
    Minimal,
    None,
}

fn draw_mode() -> DrawMode {
    thread_local! {
        static MODE: DrawMode = match std::env::var("MP_VIZ_DRAW").as_deref() {
            Ok("none") => DrawMode::None,
            Ok("minimal") => DrawMode::Minimal,
            _ => DrawMode::Full,
        };
    }
    MODE.with(|m| *m)
}

/// Where it is drawn now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Behind,
    Panel,
    Page,
    Preview,
}

/// What the brushes were made for: rebuilt when any of it changes.
struct BrushKey {
    look: Look,
    stage: Stage,
    top: f32,
    refl: f32,
    accent: [u8; 3],
    palette: Vec<[u8; 3]>,
    alpha: f32,
}

struct Viz {
    #[cfg(target_os = "macos")]
    native: macos::Presenter,
    timer: Timer,
    tap: Arc<Tap>,
    analyzer: Analyzer,
    samples: Vec<f32>,
    draw: Draw,
    /// The kicks and the tempo, for the looks that move with them.
    beat: crate::visualizer::beat::Tracker,
    last: Option<Instant>,
    brushes: Option<BrushKey>,
    /// The playing art's stem, and its palette once worked out (for Cover colours).
    art: Option<String>,
    palette: Vec<[u8; 3]>,
    palette_of: Option<String>,
    palette_gen: u64,
    /// For the bench: (frame start, time spent on it), while recording.
    stats: Option<Vec<(Instant, Duration)>>,
}

thread_local! {
    static VIZ: RefCell<Option<Viz>> = const { RefCell::new(None) };
}

pub fn install(st: &Rc<State>) {
    VIZ.with(|v| {
        *v.borrow_mut() = Some(Viz {
            #[cfg(target_os = "macos")]
            native: macos::Presenter::default(),
            timer: Timer::default(),
            tap: st.player.spectrum(),
            analyzer: Analyzer::new(Config::default()),
            samples: Vec::new(),
            draw: Draw::default(),
            beat: crate::visualizer::beat::Tracker::default(),
            last: None,
            brushes: None,
            art: None,
            palette: Vec::new(),
            palette_of: None,
            palette_gen: 0,
            stats: None,
        });
    });
    publish(st);
}

// ---- Settings ---------------------------------------------------------------------------

/// The system asks for less motion (Windows' "Show animations in Windows" off; on Linux
/// the desktop's reduced-motion or animation settings): the visualizer's default look is
/// the calm one (`platform::system::reduced_motion`).
pub fn reduced_motion() -> bool {
    crate::platform::system::reduced_motion()
}

thread_local! {
    static REDUCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Read once per run (a change in the system's setting applies at the next start).
/// `MP_REDUCED_MOTION=1` (or 0) stands in for that setting, for tests and the bench.
fn reduced() -> bool {
    REDUCED.with(|r| {
        if let Some(v) = r.get() {
            return v;
        }
        let v = std::env::var("MP_REDUCED_MOTION").map(|v| v == "1").unwrap_or_else(|_| reduced_motion());
        r.set(Some(v));
        v
    })
}

/// The settings in effect: as saved, or (never chosen) the default for this system.
pub fn settings(st: &State) -> VizSettings {
    st.settings.borrow().visualizer_look.clone().unwrap_or_else(|| {
        let mut s = VizSettings::default();
        if reduced() {
            s.choose(Preset::Calm, &ThemeViz::default());
        }
        s
    })
}

/// The placement can be checked on each frame without copying the saved colour palette.
fn placement(st: &State) -> Placement {
    st.settings.borrow().visualizer_look.as_ref().map_or(Placement::Behind, |s| s.placement)
}

fn theme_viz(app: &AppWindow) -> ThemeViz {
    let t = app.global::<Theme>();
    ThemeViz {
        opacity: t.get_viz_opacity(),
        height: t.get_viz_height(),
        bar_width: t.get_viz_bar_width(),
        bar_gap: t.get_viz_bar_gap(),
        reflection: t.get_viz_reflection(),
        reflection_opacity: t.get_viz_reflection_opacity(),
        peaks: t.get_viz_peaks(),
    }
}

/// The look in effect.
fn look(st: &State, app: &AppWindow) -> Look {
    let theme = theme_viz(app);
    match &st.settings.borrow().visualizer_look {
        Some(s) => s.effective(&theme),
        None if reduced() => Preset::Calm.look(&theme),
        None => Look::classic(&theme),
    }
}

/// The look drawn on a stage: the one in effect, or on Settings' preview a pasted or
/// imported preset waiting for the user's yes (`presets::trial`).
fn drawn(st: &State, app: &AppWindow, at: Option<Where>) -> Look {
    if at == Some(Where::Preview) && let Some(trial) = crate::visualizer::presets::trial_look() {
        return trial;
    }
    look(st, app)
}

pub(crate) fn theme(app: &AppWindow) -> ThemeViz {
    theme_viz(app)
}

/// The user's presets, as saved.
pub(crate) fn users(st: &State) -> Vec<look::UserLook> {
    st.settings.borrow().visualizer_presets.list.clone()
}

/// Change the settings, save them, show them, redraw. A preset waiting in the preview gives
/// way: the user changed the look itself.
pub(crate) fn update(st: &Rc<State>, f: impl FnOnce(&mut VizSettings, &ThemeViz)) {
    let Some(app) = st.app.upgrade() else { return };
    crate::visualizer::presets::drop_trial(st);
    let t = theme_viz(&app);
    let mut s = settings(st);
    f(&mut s, &t);
    st.settings.borrow_mut().visualizer_look = Some(s);
    st.save_settings_soon();
    publish(st);
    restyle(st);
}

/// Settings > Visualizer and the menus: a preset by its index in the picker's list (the
/// built-in ones, `Preset::ALL`, then the user's).
pub fn choose_preset(st: &Rc<State>, i: usize) {
    if let Some(&p) = Preset::ALL.get(i) {
        update(st, |s, t| s.choose(p, t));
    } else if let Some(u) = users(st).get(i - Preset::ALL.len()) {
        update(st, |s, _| s.choose_user(u));
    }
}

/// A pick's name, as the pill and the picker say it.
fn pick_name(p: Pick, users: &[look::UserLook]) -> String {
    match p {
        Pick::BuiltIn(p) => p.label().to_owned(),
        Pick::User(i) => users.get(i).map(|u| u.name.clone()).unwrap_or_default(),
    }
}

/// The key that cycles the looks: the next one (the built-in presets, then the user's), said
/// in the pill; it turns the visualizer on if it was off.
pub fn next_look(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let us = users(st);
    let p = settings(st).next_pick(&theme_viz(&app), &us);
    match p {
        Pick::BuiltIn(b) => update(st, |s, t| s.choose(b, t)),
        Pick::User(i) => update(st, |s, _| s.choose_user(&us[i])),
    }
    if !st.settings.borrow().visualizer {
        crate::toggle_visualizer(st);
    }
    crate::views::flash(st, &format!("Visualizer: {}", pick_name(p, &us)));
}

/// The key that cycles where it shows: behind the page, in the panel (its tab chosen, or
/// nothing would show), full page, and round again; with the visualizer off, it comes on
/// where it was (`look::cycle_placement`). Said in the pill, with where it shows when
/// that is not the page in view.
pub fn cycle_placement(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let was_on = st.settings.borrow().visualizer;
    let (_, p) = look::cycle_placement(was_on, settings(st).placement);
    if was_on {
        update(st, |s, _| s.placement = p);
    }
    if p == Placement::Panel {
        app.global::<Visualizer>().set_tab(true);
    }
    if !was_on {
        crate::toggle_visualizer(st);
    }
    placement_changed(st);
    let here = app.global::<Nav>().get_page() == Page::NowPlaying;
    let words = if was_on { p.words().to_owned() } else { format!("on, {}", p.words()) };
    crate::views::flash(st, &format!("Visualizer: {words}{}", if here { "" } else { " (on Now playing)" }));
}

/// One choice of the look, by what it is ("style", "colours", "colour_by", "detail",
/// "motion", "placement", "fps") and the index of the option.
pub fn choose(st: &Rc<State>, what: &str, i: usize) {
    update(st, |s, t| match what {
        "placement" => s.placement = Placement::ALL.get(i).copied().unwrap_or_default(),
        "fps" => s.fps = if i == 1 { Fps::Half } else { Fps::Full },
        _ => s.change(t, |l| match what {
            "style" => l.style = Style::ALL.get(i).copied().unwrap_or_default(),
            // The kind of ring (Settings' second choice once the style is Ring).
            "ring" => l.style = Style::RING.get(i).copied().unwrap_or(Style::Radial),
            "colours" => l.colours = Colours::ALL.get(i).copied().unwrap_or_default(),
            "colour_by" => l.colour_by = if i == 1 { ColourBy::Frequency } else { ColourBy::Height },
            "detail" => l.detail = Detail::ALL.get(i).copied().unwrap_or_default(),
            "motion" => l.motion = Motion::ALL.get(i).copied().unwrap_or_default(),
            _ => {}
        }),
    });
    if what == "placement" {
        placement_changed(st);
    }
    if what == "fps" {
        // A new rate: the timer starts again at it.
        VIZ.with(|v| {
            if let Some(z) = v.borrow_mut().as_mut() {
                z.timer.stop();
            }
        });
        wake(st);
    }
}

/// A switch of the look: "reflection", "peaks", "mirror".
pub fn toggle(st: &Rc<State>, what: &str) {
    update(st, |s, t| {
        s.change(t, |l| match what {
            "reflection" => l.reflection = !l.reflection,
            "peaks" => l.peaks = !l.peaks,
            "mirror" => l.mirror = !l.mirror,
            "cover_pulse" => l.cover_pulse = !l.cover_pulse,
            _ => {}
        })
    });
}

/// A stepper: "size" and "strength" (the reflection's, 5% steps), "sensitivity" (3 dB),
/// "intensity" (the ring's swing, 10% steps from 50% to 200%),
/// "opacity" (5%: where it shows now, behind the page or elsewhere).
pub fn step(st: &Rc<State>, what: &str, by: i32) {
    let behind = settings(st).placement == Placement::Behind;
    update(st, |s, t| {
        s.change(t, |l| {
            let pct = |v: f32, step: f32, lo: f32, hi: f32| (((v * 100.0 / step).round() + by as f32) * step / 100.0).clamp(lo, hi);
            match what {
                "size" => l.reflection_size = pct(l.reflection_size, 5.0, 0.1, 1.0),
                "strength" => l.reflection_strength = pct(l.reflection_strength, 5.0, 0.05, 1.0),
                "sensitivity" => l.sensitivity = (l.sensitivity + 3 * by).clamp(-12, 12),
                "intensity" => l.intensity = pct(l.intensity, 10.0, 0.5, 2.0),
                "opacity" | "prominence" if behind => l.prominence = pct(l.prominence, 5.0, 0.1, 1.0),
                "opacity" => l.opacity = pct(l.opacity, 5.0, 0.1, 1.0),
                _ => {}
            }
        })
    });
}

/// The quick menu's opacity: a value in percent, where it shows now.
pub fn set_opacity(st: &Rc<State>, pct: u32) {
    let behind = settings(st).placement == Placement::Behind;
    let v = (pct.clamp(10, 100) as f32) / 100.0;
    update(st, |s, t| {
        s.change(t, |l| {
            if behind {
                l.prominence = v;
            } else {
                l.opacity = v;
            }
        })
    });
    crate::views::flash(st, &format!("Visualizer opacity {pct}%"));
}

/// The custom gradient's colour `slot` (0 bottom, 2 top), a swatch on either way.
pub fn custom_step(st: &Rc<State>, slot: usize, by: i32) {
    update(st, |s, t| {
        s.change(t, |l| {
            l.colours = Colours::Custom;
            let n = look::SWATCHES.len() as i32;
            if let Some(c) = l.custom.get_mut(slot) {
                let i = (look::swatch_of(c) as i32 + by).rem_euclid(n) as usize;
                *c = look::hex(look::SWATCHES[i].1);
            }
        })
    });
}

/// Settings > Visualizer > Reset: Classic (or Calm, with Windows' animations off),
/// behind the page, at the display's rate. The user's presets stay.
pub fn reset(st: &Rc<State>) {
    crate::visualizer::presets::drop_trial(st);
    st.settings.borrow_mut().visualizer_look = None;
    st.save_settings_soon();
    publish(st);
    placement_changed(st);
    restyle(st);
    crate::views::flash(st, "Visualizer reset");
}

fn placement_changed(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    if settings(st).placement != Placement::Panel {
        app.global::<Visualizer>().set_tab(false);
    }
    wake(st);
}

fn pct_text(v: f32) -> String {
    format!("{}%", (v * 100.0).round())
}

/// The settings as the UI shows them (Settings > Visualizer, the stage's layout).
pub fn publish(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = settings(st);
    let t = theme_viz(&app);
    let l = s.effective(&t);
    let us = users(st);
    let g = app.global::<Visualizer>();
    g.set_placement(Placement::ALL.iter().position(|p| *p == s.placement).unwrap_or(0) as i32);
    let current = s.current(&t, &us);
    g.set_preset(match current {
        Some(Pick::BuiltIn(p)) => Preset::ALL.iter().position(|q| *q == p).map_or(-1, |i| i as i32),
        Some(Pick::User(i)) => (Preset::ALL.len() + i) as i32,
        None => -1,
    });
    let based = s.based_on.clone().filter(|n| us.iter().any(|u| &u.name == n));
    g.set_look_name(match current {
        Some(p) => pick_name(p, &us),
        None => "Custom".to_owned(),
    }
    .into());
    g.set_user_current(match current {
        Some(Pick::User(i)) => us[i].name.clone().into(),
        _ => "".into(),
    });
    // Changed since it was chosen: "Update" saves the change to it.
    g.set_user_based(if current.is_none() { based.clone().unwrap_or_default().into() } else { "".into() });
    let mut presets: Vec<crate::VizPresetRow> = Preset::ALL.iter().map(|p| crate::VizPresetRow { name: p.label().into(), hint: p.describe().into(), user: false }).collect();
    presets.extend(us.iter().map(|u| crate::VizPresetRow { name: u.name.clone().into(), hint: describe(&u.look).into(), user: true }));
    let same = g.get_presets().iter().eq(presets.iter().cloned());
    if !same {
        g.set_presets(ModelRc::new(VecModel::from(presets)));
    }
    g.set_user_count(us.len() as i32);
    // The style as Settings offers it (every kind of ring is "Ring"), and the kind of ring.
    g.set_style(Style::TOP.iter().position(|x| *x == l.style).unwrap_or(2) as i32);
    g.set_ring_kind(Style::RING.iter().position(|x| *x == l.style).map_or(-1, |i| i as i32));
    g.set_cover_pulse(l.cover_pulse);
    g.set_colours(Colours::ALL.iter().position(|x| *x == l.colours).unwrap_or(0) as i32);
    g.set_colour_by(if l.colour_by == ColourBy::Frequency { 1 } else { 0 });
    g.set_detail(Detail::ALL.iter().position(|x| *x == l.detail).unwrap_or(1) as i32);
    g.set_motion(Motion::ALL.iter().position(|x| *x == l.motion).unwrap_or(1) as i32);
    g.set_fps(if s.fps == Fps::Half { 1 } else { 0 });
    g.set_reflection(l.reflection);
    g.set_peaks(l.peaks);
    g.set_mirror(l.mirror);
    g.set_reflects(l.style.reflects());
    g.set_has_peaks(l.style.has_peaks());
    g.set_can_mirror(matches!(l.style, Style::Bars | Style::Line));
    g.set_refl_size(pct_text(l.reflection_size).into());
    g.set_refl_strength(pct_text(l.reflection_strength).into());
    g.set_sensitivity(if l.sensitivity == 0 { "0 dB".into() } else { format!("{:+} dB", l.sensitivity).into() });
    g.set_intensity(pct_text(l.intensity).into());
    g.set_opacity(pct_text(l.strength(s.placement == Placement::Behind)).into());
    g.set_opacity_behind(pct_text(l.prominence).into());
    g.set_opacity_elsewhere(pct_text(l.opacity).into());
    g.set_refl_share(if l.reflection && l.style.reflects() { l.reflection_size } else { 0.0 });
    let names: Vec<SharedString> = l.custom.iter().map(|c| look::SWATCHES[look::swatch_of(c)].0.into()).collect();
    let cols: Vec<slint::Color> = l.custom.iter().map(|c| look::parse_hex(c).unwrap_or([255; 3])).map(|[r, gr, b]| slint::Color::from_rgb_u8(r, gr, b)).collect();
    g.set_custom_names(ModelRc::new(VecModel::from(names)));
    g.set_custom(ModelRc::new(VecModel::from(cols)));
    let drawn = crate::visualizer::presets::trial_look().unwrap_or_else(|| l.clone());
    g.set_ring(drawn.style.around());
    g.set_tall(drawn.style.tall());
    swatches(&app, &l);
}

/// A look in a few words, for a user preset's tooltip: "Ring, cover colours".
pub fn describe(l: &Look) -> String {
    format!("{}, {} colours", l.style.full_label(), l.colours.label().to_lowercase())
}

/// The colour schemes' swatches (the cover's change with the track).
fn swatches(app: &AppWindow, l: &Look) {
    let accent = rgb(app.global::<Theme>().get_accent());
    let palette = VIZ.with(|v| v.borrow().as_ref().map(|z| z.palette.clone()).unwrap_or_default());
    let b: Vec<Brush> = Colours::ALL.iter().map(|&c| draw::swatch(l, c, accent, &palette)).collect();
    let g = app.global::<Visualizer>();
    let same = g.get_swatches().iter().eq(b.iter().cloned());
    if !same {
        g.set_swatches(ModelRc::new(VecModel::from(b)));
    }
}

fn rgb(c: slint::Color) -> [u8; 3] {
    [c.red(), c.green(), c.blue()]
}

/// theme.toml was (re)loaded: Classic may look different now.
pub fn theme_changed() {
    let _ = state::with(|st| {
        publish(st);
        restyle(st);
    });
}

/// The accent moved (a new track's cover): the brushes follow, also on a still stage.
pub fn accent_changed() {
    let _ = state::with(|st| {
        let running = VIZ.with(|v| v.borrow().as_ref().is_some_and(|z| z.timer.running()));
        if !running {
            restyle(st);
        }
        if let Some(app) = st.app.upgrade()
            && app.global::<Nav>().get_page() == Page::Settings
        {
            swatches(&app, &look(st, &app));
        }
    });
}

/// The playing track's art changed: its palette is worked out off the UI thread, when the
/// look uses the cover's colours (otherwise when it first does).
pub fn follow_art(st: &Rc<State>, stem: Option<String>) {
    VIZ.with(|v| {
        if let Some(z) = v.borrow_mut().as_mut() {
            z.art = stem;
        }
    });
    fetch_palette(st);
}

fn fetch_palette(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    // The Settings page shows every scheme's swatch, the cover's too.
    let wanted = look(st, &app).colours == Colours::Cover || app.global::<Nav>().get_page() == Page::Settings;
    let job = VIZ.with(|v| {
        let mut v = v.borrow_mut();
        let z = v.as_mut()?;
        if z.palette_of == z.art {
            return None;
        }
        let Some(stem) = z.art.clone() else {
            z.palette.clear();
            z.palette_of = None;
            return None;
        };
        if !wanted {
            return None;
        }
        z.palette_gen += 1;
        Some((stem, z.palette_gen))
    });
    let Some((stem, generation)) = job else { return };
    let cache = st.art.clone();
    std::thread::spawn(move || {
        let p = cache.palette(&stem);
        let _ = slint::invoke_from_event_loop(move || {
            let fresh = VIZ.with(|v| {
                let mut v = v.borrow_mut();
                let Some(z) = v.as_mut() else { return false };
                if z.palette_gen != generation {
                    return false;
                }
                z.palette = p;
                z.palette_of = Some(stem);
                true
            });
            if fresh {
                let _ = state::with(|st| {
                    restyle(st);
                    if let Some(app) = st.app.upgrade() {
                        swatches(&app, &look(st, &app));
                    }
                });
            }
        });
    });
}

// ---- Running ----------------------------------------------------------------------------

/// Where it is to be drawn now, if anywhere.
fn stage(st: &State, app: &AppWindow) -> Option<Where> {
    if !crate::ticker::shown(app.window()) {
        return None;
    }
    match app.global::<Nav>().get_page() {
        Page::NowPlaying if st.settings.borrow().visualizer => match placement(st) {
            Placement::Behind => Some(Where::Behind),
            Placement::Panel => app.global::<Visualizer>().get_tab().then_some(Where::Panel),
            Placement::Page => Some(Where::Page),
        },
        Page::Settings if app.global::<SettingsView>().get_section() == crate::settings::nav::VISUALIZER as i32 && app.global::<SettingsView>().get_filter().is_empty() => Some(Where::Preview),
        _ => None,
    }
}

/// Settings > Visualizer is shown (not the filter's results over it).
pub fn shown(st: &State) -> bool {
    st.app.upgrade().is_some_and(|app| {
        app.global::<Nav>().get_page() == Page::Settings
            && app.global::<SettingsView>().get_section() == crate::settings::nav::VISUALIZER as i32
            && app.global::<SettingsView>().get_filter().is_empty()
    })
}

/// Music is being heard: playing, and not waiting for a server track's download (nothing to
/// draw then).
fn playing(st: &State) -> bool {
    let s = st.player.status();
    s.state == PlayState::Playing && !s.buffering
}

/// Something changed that may start or stop it: playback, the page, the window, the
/// setting, the stage's size. Cheap; called on every playback tick.
pub fn wake(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let at = stage(st, &app);
    // A preset waiting in the preview goes with the preview.
    if at != Some(Where::Preview) && crate::visualizer::presets::trial_look().is_some() && !shown(st) {
        crate::visualizer::presets::drop_trial(st);
    }
    let play = playing(st);
    let mut still = false;
    VIZ.with(|v| {
        let mut v = v.borrow_mut();
        let Some(z) = v.as_mut() else { return };
        match at {
            Some(_) if play && !z.timer.running() => {
                z.tap.set_active(true);
                z.last = None;
                let refresh = app.window().with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz())).flatten().map(|mhz| (mhz + 500) / 1000);
                let every = look::frame_interval(settings(st).fps, refresh);
                #[cfg(target_os = "macos")]
                { z.native.display_interval = every; }
                z.timer.start(TimerMode::Repeated, every, || state::with(frame).unwrap_or(()));
            }
            Some(Where::Preview) if !play && !z.timer.running() => still = true,
            // The preview's still frame, left on another stage (Settings closed onto Now
            // playing, nothing playing): put away, as music at rest would be.
            Some(_) if !play && !z.timer.running() && app.global::<Visualizer>().get_still() => stop(z, &app),
            None if z.timer.running() || app.global::<Visualizer>().get_active() => stop(z, &app),
            _ => {}
        }
    });
    if still {
        still_frame(st, &app);
    }
    // A menu or an unpinned sidebar may open between spectrum samples. Hand the
    // complete scene to Skia before the overlay draws a backdrop filter over it.
    #[cfg(target_os = "macos")]
    if at.is_some() && app.global::<Visualizer>().get_native_underlay()
        && app.global::<Theme>().get_glass() && !app.get_viz_fixed_glass()
    {
        frame(st);
    }
}

fn stop(z: &mut Viz, app: &AppWindow) {
    #[cfg(target_os = "macos")]
    z.native.remove(app);
    z.timer.stop();
    z.tap.set_active(false);
    z.analyzer.reset();
    z.last = None;
    let g = app.global::<Visualizer>();
    g.set_active(false);
    g.set_still(false);
    g.set_cover_scale(1.0);
    z.draw.reset();
    z.beat.reset();
    set_paths(&g, &z.draw.paths);
}

fn set_paths(g: &Visualizer, p: &draw::Paths) {
    g.set_fill(p.fill.as_str().into());
    g.set_caps(p.caps.as_str().into());
    g.set_line(p.line.as_str().into());
    g.set_refl0(p.refl[0].as_str().into());
    g.set_refl1(p.refl[1].as_str().into());
    g.set_refl2(p.refl[2].as_str().into());
    g.set_refl3(p.refl[3].as_str().into());
    g.set_refl4(p.refl[4].as_str().into());
    g.set_refl5(p.refl[5].as_str().into());
    g.set_top_h(p.top_h);
    g.set_refl_h(p.refl_h);
    g.set_ripples(p.ripples.as_str().into());
    g.set_sparks(p.sparks.as_str().into());
    g.set_sparks_old(p.sparks_old.as_str().into());
    g.set_sparks_glow(p.sparks_glow.as_str().into());
    g.set_front(p.front.as_str().into());
    g.set_front_glow(p.front_glow.as_str().into());
    // The ring's bloom: faint at rest, brighter with the low notes.
    let pulse = 0.4 + 0.6 * p.pulse.clamp(0.0, 1.0);
    if (g.get_pulse() - pulse).abs() > 0.004 {
        g.set_pulse(pulse);
    }
}

fn set_brushes(g: &Visualizer, b: &Brushes) {
    g.set_fill_brush(b.fill.clone());
    g.set_caps_brush(b.caps.clone());
    g.set_line_brush(b.line.clone());
    g.set_glow_brush(b.glow.clone());
    g.set_refl_brush0(b.refl[0].clone());
    g.set_refl_brush1(b.refl[1].clone());
    g.set_refl_brush2(b.refl[2].clone());
    g.set_refl_brush3(b.refl[3].clone());
    g.set_refl_brush4(b.refl[4].clone());
    g.set_refl_brush5(b.refl[5].clone());
    g.set_bloom_brush(b.bloom.clone());
    g.set_ripples_brush(b.ripples.clone());
    g.set_sparks_brush(b.sparks.clone());
    g.set_sparks_old_brush(b.sparks_old.clone());
    g.set_sparks_glow_brush(b.sparks_glow.clone());
    g.set_ripples_width(b.ripple_width);
    g.set_line_width(b.line_width);
    g.set_glow_width(b.glow_width);
}

/// The halo follows the cover's rounded square instead of a circle (`MP_HALO_SHAPE=square`,
/// for comparing the two; the round one is the design).
fn halo_square() -> bool {
    thread_local! {
        static SQUARE: bool = std::env::var("MP_HALO_SHAPE").is_ok_and(|v| v == "square");
    }
    SQUARE.with(|s| *s)
}

fn stage_of(app: &AppWindow) -> Stage {
    let g = app.global::<Visualizer>();
    let t = app.global::<Theme>();
    Stage {
        hub: g.get_stage_hub().max(0.0),
        corner: if g.get_square() { t.get_radius_card() } else { -1.0 },
        ..Stage::new(g.get_stage_width().max(1.0), g.get_stage_height().max(1.0), (t.get_viz_bar_width(), t.get_viz_bar_gap()))
    }
}

/// The brushes for this frame, rebuilt when what they depend on changed.
fn brush(z: &mut Viz, app: &AppWindow, l: &Look, s: &Stage, at: Where, behind: bool) -> bool {
    let accent = rgb(app.global::<Theme>().get_accent());
    let alpha = l.strength(at == Where::Behind || (at == Where::Preview && behind));
    let (top, refl) = (z.draw.paths.top_h, z.draw.paths.refl_h);
    if z.brushes.as_ref().is_some_and(|key|
        &key.look == l && &key.stage == s && key.top == top && key.refl == refl
            && key.accent == accent && key.palette == z.palette && key.alpha == alpha)
    {
        return false;
    }
    let stops = draw::base_stops(l, accent, &z.palette);
    let b = draw::brushes(l, s, top, refl, &stops, alpha);
    set_brushes(&app.global::<Visualizer>(), &b);
    app.global::<Visualizer>().set_crisp(draw::crisp(l));
    z.brushes = Some(BrushKey { look: l.clone(), stage: *s, top, refl, accent, palette: z.palette.clone(), alpha });
    true
}

/// The look, the colours or the theme changed: redraw what is shown, running or not.
pub fn restyle(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let at = stage(st, &app);
    let l = drawn(st, &app, at);
    app.global::<Visualizer>().set_ring(l.style.around());
    app.global::<Visualizer>().set_tall(l.style.tall());
    app.global::<Visualizer>().set_square(halo_square());
    let cfg = look::analysis(&l);
    VIZ.with(|v| {
        if let Some(z) = v.borrow_mut().as_mut() {
            z.analyzer.set_config(cfg);
            z.brushes = None;
        }
    });
    fetch_palette(st);
    let running = VIZ.with(|v| v.borrow().as_ref().is_some_and(|z| z.timer.running()));
    if running {
        return;
    }
    match at {
        Some(Where::Preview) if !playing(st) => still_frame(st, &app),
        Some(_) => wake(st),
        None => {}
    }
}

/// Made-up music for the preview while nothing plays: a kick's weight low, a dip in the
/// low mids, the voice and the snare's body, the treble falling away, with the unevenness
/// of a real moment (a few slow waves across the bands, and some grain).
pub fn still_levels(bands: usize) -> (Vec<f32>, Vec<f32>) {
    let a = Analyzer::new(Config::default());
    let centres = a.centres(bands.max(1));
    let mut seed = 0x2545_f491u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 1000) as f32 / 1000.0
    };
    let bump = |o: f32, at: f32, w: f32| (-((o - at) / w).powi(2)).exp();
    let levels: Vec<f32> = centres
        .iter()
        .map(|&f| {
            // Octaves above 30 Hz: 0 to about 9.
            let o = (f / 30.0).log2();
            let shape = 0.22 + 0.62 * bump(o, 1.1, 0.75) + 0.3 * bump(o, 2.3, 0.6) + 0.34 * bump(o, 5.2, 1.1) + 0.2 * bump(o, 6.9, 0.5) - 0.05 * (o - 6.0).max(0.0);
            let waves = 0.07 * (o * 3.1).sin() + 0.05 * (o * 7.3 + 1.0).sin();
            (shape + waves + (rand() - 0.5) * 0.12).clamp(0.04, 0.9)
        })
        .collect();
    let peaks = levels.iter().map(|&l| (l + 0.02 + rand() * 0.09).min(0.97)).collect();
    (levels, peaks)
}

/// The preview with nothing playing: one frame, drawn once.
fn still_frame(st: &Rc<State>, app: &AppWindow) {
    let s = settings(st);
    let l = drawn(st, app, Some(Where::Preview));
    let sg = stage_of(app);
    let g = app.global::<Visualizer>();
    VIZ.with(|v| {
        let mut v = v.borrow_mut();
        let Some(z) = v.as_mut() else { return };
        let (_, bands) = draw::layout(&l, &sg);
        let (levels, peaks) = still_levels(bands);
        z.draw.draw(&l, &sg, true, &levels, &peaks);
        brush(z, app, &l, &sg, Where::Preview, s.placement == Placement::Behind);
        set_paths(&g, &z.draw.paths);
    });
    g.set_still(true);
    g.set_active(true);
}

fn frame(st: &Rc<State>) {
    crate::wakes::note(&crate::wakes::VIZ_FRAMES);
    let Some(app) = st.app.upgrade() else { return };
    let at = stage(st, &app);
    let play = playing(st);
    let l = drawn(st, &app, at);
    let sg = stage_of(&app);
    let mut back_to_still = false;
    VIZ.with(|v| {
        let mut v = v.borrow_mut();
        let Some(z) = v.as_mut() else { return };
        let Some(at) = at else {
            stop(z, &app);
            return;
        };
        let started = Instant::now();
        let dt = z.last.map_or(1.0 / 60.0, |t| (started - t).as_secs_f32()).min(0.1);
        z.last = Some(started);
        let (_, bands) = draw::layout(&l, &sg);
        let rate = z.tap.rate();
        if play {
            z.tap.set_active(true);
            z.samples.resize(Analyzer::fft_size(rate), 0.0);
            let rate = z.tap.read(&mut z.samples, true);
            z.analyzer.update(Some((&z.samples, rate)), bands, dt);
            // The beat, for the looks that move with it: listened for in the same samples.
            if l.style.beats() {
                let heard = z.beat.update(&z.samples, rate, dt);
                z.draw.pulse(draw::Pulse { hit: heard.hit, snap: heard.snap, period: heard.period });
                #[cfg(feature = "bench")]
                HEARD.with(|h| {
                    let (bpm, hits) = h.get();
                    h.set((if heard.period > 0.0 { heard.bpm() } else { 0.0 * bpm }, hits + u32::from(heard.hit > 0.0)));
                });
            }
        } else {
            // Paused or stopped: no more samples; the bars fall.
            z.tap.set_active(false);
            z.analyzer.update(None, bands, dt);
            z.beat.rest(dt);
            z.draw.pulse(draw::Pulse::default());
        }
        let g = app.global::<Visualizer>();
        match draw_mode() {
            #[cfg(target_os = "macos")]
            DrawMode::Full if macos::present(z, &app, &l, &sg, at, dt) => {}
            DrawMode::Full => {
                #[cfg(target_os = "macos")]
                z.native.restore_interval(&z.timer);
                #[cfg(target_os = "macos")]
                z.native.remove(&app);
                z.draw.tick(dt);
                z.draw.draw(&l, &sg, true, z.analyzer.levels(), z.analyzer.peaks());
                brush(z, &app, &l, &sg, at, placement(st) == Placement::Behind);
                set_paths(&g, &z.draw.paths);
                // The ring's cover, a little larger for a moment with each beat.
                let scale = if l.style.around() && l.cover_pulse { 1.0 + COVER_PULSE * z.draw.paths.punch.clamp(0.0, 1.0) } else { 1.0 };
                if let Some(s) = cover_scale_step(g.get_cover_scale(), scale) {
                    g.set_cover_scale(s);
                }
            }
            // Measurement only: the window redrawn each frame for a 1 px rectangle.
            DrawMode::Minimal => g.set_fill(if g.get_frame() % 2 == 0 { "M0 0H1V1H0Z" } else { "M0 0H2V1H0Z" }.into()),
            // Measurement only: the analysis without drawing.
            DrawMode::None => {}
        }
        g.set_active(true);
        g.set_still(false);
        g.set_frame(g.get_frame().wrapping_add(1));
        if let Some(s) = z.stats.as_mut() {
            s.push((started, started.elapsed()));
        }
        if !play && z.analyzer.at_rest() {
            stop(z, &app);
            back_to_still = at == Where::Preview;
        }
    });
    if back_to_still {
        still_frame(st, &app);
    }
}

// ---- The quick menu ---------------------------------------------------------------------

/// How much larger the ring's cover is at the top of a beat (a tiny bit: 2.5%).
const COVER_PULSE: f32 = 0.025;

/// The opacity steps the quick menu offers.
pub const OPACITY_STEPS: [u32; 10] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10];

/// Now playing's visualizer menu (its button, or a right click on the page): on and off,
/// the opacity where it shows, then under a heading each the presets (the built-in ones,
/// and the user's in a submenu) and where it shows, and Settings. What a look is made of
/// (its style, its colours) is Settings' business: here a look is chosen whole.
pub fn menu_entries(st: &State, app: &AppWindow) -> Vec<crate::menus::Entry> {
    use crate::menus::{Act, Entry};
    let s = settings(st);
    let t = theme_viz(app);
    let l = s.effective(&t);
    let us = users(st);
    let current = s.current(&t, &us);
    let on = st.settings.borrow().visualizer;
    let pick = |label: &str, act: Act, checked: bool| if checked { Entry::Checked { label: label.into(), act } } else { Entry::Item { label: label.into(), act } };
    let mut v = vec![pick("Visualizer", Act::VizToggle, on)];
    let now = (l.strength(s.placement == Placement::Behind) * 100.0).round() as u32;
    v.push(Entry::Choices {
        label: format!("Opacity: {now}%"),
        children: OPACITY_STEPS.iter().map(|&p| pick(&format!("{p}%"), Act::VizOpacity(p), p == now)).collect(),
    });
    v.push(Entry::Separator);
    v.push(Entry::Heading { label: "Presets".into() });
    v.extend(Preset::ALL.iter().enumerate().map(|(i, p)| pick(p.label(), Act::VizPreset(i), current == Some(Pick::BuiltIn(*p)))));
    if !us.is_empty() {
        let mine = match current {
            Some(Pick::User(i)) => format!("Your presets: {}", us[i].name),
            _ => "Your presets".to_owned(),
        };
        v.push(Entry::Choices {
            label: mine,
            children: us.iter().enumerate().map(|(i, u)| pick(&u.name, Act::VizPreset(Preset::ALL.len() + i), current == Some(Pick::User(i)))).collect(),
        });
    }
    v.push(Entry::Separator);
    v.push(Entry::Heading { label: "Where it shows".into() });
    v.extend(Placement::ALL.iter().enumerate().map(|(i, p)| pick(p.label(), Act::VizPlacement(i), *p == s.placement)));
    v.push(Entry::Separator);
    v.push(Entry::Item { label: "Visualizer settings\u{2026}".into(), act: Act::VizSettings });
    v
}

pub fn menu(st: &Rc<State>, x: f32, y: f32, from_keys: bool) {
    let Some(app) = st.app.upgrade() else { return };
    let e = menu_entries(st, &app);
    crate::menus::show_plain(st, e, x, y, from_keys);
}

#[cfg(feature = "bench")]
/// Inspect the actual presenter geometry for ring, particle and line assertions.
pub fn effect_paths(app: &AppWindow) -> draw::Paths {
    let g = app.global::<Visualizer>();
    #[cfg(target_os = "macos")]
    if g.get_native_underlay() {
        return VIZ.with(|v| v.borrow().as_ref().map(|z| z.native.paths.as_svg()).unwrap_or_default());
    }
    draw::Paths {
        line: g.get_line().to_string(), fill: g.get_fill().to_string(), caps: g.get_caps().to_string(),
        refl: [g.get_refl0(), g.get_refl1(), g.get_refl2(), g.get_refl3(), g.get_refl4(), g.get_refl5()].map(|s| s.to_string()),
        ripples: g.get_ripples().to_string(),
        sparks: g.get_sparks().to_string(), sparks_old: g.get_sparks_old().to_string(),
        sparks_glow: g.get_sparks_glow().to_string(), front: g.get_front().to_string(), front_glow: g.get_front_glow().to_string(),
        top_h: g.get_top_h(), refl_h: g.get_refl_h(),
        ..Default::default()
    }
}

#[cfg(feature = "bench")]
pub fn presented_cover_scale(app: &AppWindow) -> f32 {
    let g = app.global::<Visualizer>();
    #[cfg(target_os = "macos")]
    if g.get_native_underlay() {
        unsafe extern "C" { fn mp_spectrum_cover_scale() -> f32; }
        // SAFETY: the bench samples the attached layer on the UI thread.
        return unsafe { mp_spectrum_cover_scale() };
    }
    g.get_cover_scale()
}

#[cfg(feature = "bench")]
/// The bench: start or stop recording frame times; `take` returns them.
pub fn record(on: bool) {
    VIZ.with(|v| {
        if let Some(z) = v.borrow_mut().as_mut() {
            z.stats = on.then(Vec::new);
        }
    });
}

#[cfg(feature = "bench")]
pub fn take_stats() -> Vec<(Instant, Duration)> {
    VIZ.with(|v| v.borrow_mut().as_mut().and_then(|z| z.stats.take()).unwrap_or_default())
}

#[cfg(feature = "bench")]
/// The bench: is the timer running, and is the tap taking samples?
pub fn running() -> (bool, bool) {
    VIZ.with(|v| v.borrow().as_ref().map_or((false, false), |z| (z.timer.running(), z.tap.is_active())))
}

#[cfg(feature = "bench")]
/// The bench: the current bar levels.
pub fn levels() -> Vec<f32> {
    VIZ.with(|v| v.borrow().as_ref().map(|z| z.analyzer.levels().to_vec()).unwrap_or_default())
}

#[cfg(feature = "bench")]
thread_local! {
    /// The bench: the tempo heard (beats a minute, 0 for none) and the beats shown so far.
    static HEARD: std::cell::Cell<(f32, u32)> = const { std::cell::Cell::new((0.0, 0)) };
}

#[cfg(feature = "bench")]
/// The bench: the tempo heard now (0 for none) and how many beats have been shown.
pub fn heard() -> (f32, u32) {
    HEARD.with(std::cell::Cell::get)
}

#[cfg(feature = "bench")]
/// The bench: the palette worked out for the playing cover.
pub fn palette() -> Vec<[u8; 3]> {
    VIZ.with(|v| v.borrow().as_ref().map(|z| z.palette.clone()).unwrap_or_default())
}

/// The ring's cover scale to set, from `now` towards `target`: changes too small to see are
/// not drawn, but the cover always comes back to its own size (1.0). Within 0.0004 of it, it
/// stayed there for good (the pulse switched off, or the beat gone, as it eased back): a
/// hair larger than its frame.
fn cover_scale_step(now: f32, target: f32) -> Option<f32> {
    ((now - target).abs() > 0.0004 || (target == 1.0 && now != 1.0)).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cover_always_comes_back_to_its_size() {
        assert_eq!(cover_scale_step(1.0003, 1.0), Some(1.0), "switched off a hair above its size");
        assert_eq!(cover_scale_step(1.0, 1.0), None);
        assert_eq!(cover_scale_step(1.0100, 1.0102), None, "too small to see while pulsing");
        assert_eq!(cover_scale_step(1.0, 1.02), Some(1.02));
    }

    #[test]
    fn the_still_frame_looks_like_music() {
        let (l, p) = still_levels(120);
        assert_eq!(l.len(), 120);
        assert!(l.iter().zip(&p).all(|(l, p)| p >= l && *l > 0.0 && *l < 1.0));
        // Fuller low than high, and uneven.
        let third = |k: usize| l[k * 40..(k + 1) * 40].iter().sum::<f32>() / 40.0;
        assert!(third(0) > third(2), "{} {}", third(0), third(2));
        assert!(l.windows(2).any(|w| (w[0] - w[1]).abs() > 0.05));
        // The same every time: it is a still frame.
        assert_eq!(still_levels(64), still_levels(64));
    }
}
