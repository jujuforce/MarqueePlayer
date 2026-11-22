//! Core Animation draws the spectrum below the macOS window surface. In settled layouts,
//! native glass blurs the spectrum while Slint draws foreground content and controls.
//! Moving panels and overlays use the full Slint scene for their backdrop filters.

use std::{ffi::c_void, time::Duration};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{Brush, Color, ComponentHandle, Image};

use super::{
    Look, Stage, Viz, Where, brush, draw,
    look::{ColourBy, Colours, Style},
};
use crate::{AppWindow, Backdrop, MenuStore, Nav, Player, StageBounds, Theme, Visualizer};

#[repr(C)]
struct Stop {
    position: f32,
    argb: u32,
}

#[repr(C)]
struct Bar {
    x: f32,
    width: f32,
    height: f32,
    radius: f32,
    cap: f32,
    reflection: f32,
}

unsafe extern "C" {
    fn mp_spectrum_attach(view: *mut c_void, menu_open: bool, glass: bool) -> i32;
    fn mp_spectrum_remove();
    fn mp_spectrum_backdrop(base: u32, tint: u32, opacity: f32, rgb: *const u8, width: u32, height: u32);
    fn mp_spectrum_gradient(reflection: bool, stops: *const Stop, count: usize, angle: f32);
    fn mp_spectrum_slices(alpha: *const f32, count: usize);
    fn mp_spectrum_layout(x: f32, y: f32, width: f32, top: f32, reflection: f32, scale: f32, window_pixels: u32);
    fn mp_spectrum_bars(bars: *const Bar, count: usize, top: f32, duration: f64);
}

#[derive(PartialEq)]
struct Background {
    base: Color,
    tint: Color,
    image: Image,
    opacity: f32,
}

#[derive(Default)]
pub(super) struct Presenter {
    background: Option<Background>,
    layout: Option<([f32; 6], slint::PhysicalSize)>,
    bars: Vec<Bar>,
    pub(super) paths: draw::NativePaths,
    cover: Option<(Image, [f32; 4], bool)>,
    style: Option<Style>,
    panel: Option<([f32; 5], bool)>,
    glass: Option<Glass>,
    pub(super) display_interval: Duration,
}

fn model_frames(style: Style, display_interval: Duration) -> u32 {
    // Expanding ripples appear and expire independently. Draw their current contours
    // each display frame so a departing ring never morphs into a newly born one.
    if style == Style::Radial || display_interval >= Duration::from_millis(32) {
        1
    } else if style.beats() {
        3
    } else {
        2
    }
}

impl Presenter {
    fn interpolated_interval(&self, timer: &slint::Timer, style: Style) -> f64 {
        let frames = model_frames(style, self.display_interval);
        let model_interval = self.display_interval * frames;
        if timer.interval() != model_interval {
            timer.set_interval(model_interval);
        }
        if frames == 1 { 0.0 } else { model_interval.as_secs_f64() }
    }

    pub(super) fn restore_interval(&self, timer: &slint::Timer) {
        if timer.running() && timer.interval() != self.display_interval {
            timer.set_interval(self.display_interval);
        }
    }

    pub(super) fn remove(&mut self, app: &AppWindow) {
        if self.background.is_some() {
            // SAFETY: all presenter calls run on the UI thread.
            unsafe { mp_spectrum_remove() };
            self.background = None;
            self.layout = None;
            self.style = None;
            self.panel = None;
            self.glass = None;
            self.cover = None;
            app.global::<Visualizer>().set_native_underlay(false);
        }
    }

    fn sync_background(&mut self, app: &AppWindow) {
        let theme = app.global::<Theme>();
        let backdrop = app.global::<Backdrop>();
        let next = Background {
            base: theme.get_bg(),
            tint: theme.get_backdrop_tint(),
            image: if backdrop.get_visible() {
                backdrop.get_image()
            } else {
                Image::default()
            },
            opacity: theme.get_backdrop_opacity(),
        };
        if self.background.as_ref() == Some(&next) {
            return;
        }
        let rgb = next.image.to_rgb8();
        let (ptr, w, h) = rgb.as_ref().map_or((std::ptr::null(), 0, 0), |b| {
            (b.as_bytes().as_ptr(), b.width(), b.height())
        });
        // SAFETY: the native code copies this buffer before returning.
        unsafe {
            mp_spectrum_backdrop(
                next.base.as_argb_encoded(),
                next.tint.as_argb_encoded(),
                next.opacity,
                ptr,
                w,
                h,
            )
        };
        self.background = Some(next);
    }

    fn sync_layout(&mut self, app: &AppWindow, stage: &Stage, top: f32, reflection: f32) {
        let g = app.global::<Visualizer>();
        let dimensions = [
            g.get_stage_x(),
            g.get_stage_y(),
            stage.w,
            top,
            reflection,
            app.window().scale_factor(),
        ];
        let pixels = app.window().size();
        if self.layout == Some((dimensions, pixels)) {
            return;
        }
        let [x, y, width, top, reflection, scale] = dimensions;
        // SAFETY: geometry values are copied into the attached main-thread scene.
        unsafe { mp_spectrum_layout(x, y, width, top, reflection, scale, pixels.width) };
        self.layout = Some((dimensions, pixels));
        self.panel = None;
        self.glass = None;
    }
}

fn gradient(reflection: bool, brush: Brush) {
    if let Brush::LinearGradient(gradient) = brush {
        let stops: Vec<_> = gradient
            .stops()
            .map(|s| Stop {
                position: s.position,
                argb: s.color.as_argb_encoded(),
            })
            .collect();
        // SAFETY: Core Animation copies the gradient stops synchronously.
        unsafe { mp_spectrum_gradient(reflection, stops.as_ptr(), stops.len(), gradient.angle()) };
    }
}

pub(super) fn present(z: &mut Viz, app: &AppWindow, look: &Look, stage: &Stage, at: Where, dt: f32) -> bool {
    // Settings' preview sits inside its own scrollable surface.
    let theme = app.global::<Theme>();
    let glass = theme.get_glass();
    if (glass && !app.get_viz_fixed_glass())
        || at == Where::Preview
        || app.get_page_fade() < 1.0
        || (look.style.around()
            && (!app.global::<Player>().get_has_cover() || app.global::<Player>().get_full().size().width == 0))
    {
        return false;
    }
    let handle = app.window().window_handle();
    let Ok(RawWindowHandle::AppKit(view)) = handle.window_handle().map(|h| h.as_raw()) else {
        return false;
    };
    let menu = app.global::<MenuStore>();
    // SAFETY: AppKit's NSView comes from the live main-thread Slint window.
    let attached = unsafe { mp_spectrum_attach(view.ns_view.as_ptr(), menu.get_open(), glass) };
    if attached == 0 {
        return false;
    }
    let g = app.global::<Visualizer>();
    if attached == 2 {
        z.native.background = None;
        z.native.layout = None;
        z.native.style = None;
        super::set_paths(&g, &draw::Paths::default());
    }
    if z.native.style != Some(look.style) {
        // SAFETY: all layers and the scene belong to this UI thread.
        unsafe { mp_spectrum_clear_geometry() };
        z.brushes = None;
        z.native.cover = None;
        z.native.style = Some(look.style);
    }
    let (top, reflection) = draw::split(look, stage.h, true);
    z.draw.paths.top_h = top;
    z.draw.paths.refl_h = reflection;
    let changed = brush(z, app, look, stage, at, at == Where::Behind);
    z.native.sync_background(app);
    z.native.sync_layout(app, stage, top, reflection);
    let pill = app.get_viz_pill_frame();
    let nav = app.global::<Nav>();
    let panes = Glass {
        blur: theme.get_glass_radius_blur(),
        left: if nav.get_sidebar_pinned() { theme.get_sidebar_width() } else { 0.0 },
        right: if nav.get_queue_pinned() { theme.get_queue_width() } else { 0.0 },
        bar: if app.get_bar_visible() { theme.get_bar_height() } else { 0.0 },
        pill: pill.into(),
        menu: if menu.get_open() { menu.get_main_frame().into() } else { Rect::default() },
        submenu: if menu.get_open() && menu.get_sub_of() >= 0 { menu.get_sub_frame().into() } else { Rect::default() },
        menu_radius: theme.get_radius_card(),
    };
    if z.native.glass != Some(panes) {
        // SAFETY: the scene owns the filters; it copies scalar layout values synchronously.
        unsafe { mp_spectrum_glass(panes) };
        z.native.glass = Some(panes);
    }
    let frame = g.get_panel_frame();
    let panel = if at == Where::Panel {
        (
            [
                frame.x,
                frame.y,
                frame.width,
                frame.height,
                app.global::<Theme>().get_radius_card(),
            ],
            look.style.around(),
        )
    } else {
        ([0.0; 5], false)
    };
    if z.native.panel != Some(panel) {
        let ([x, y, w, h, radius], clip) = panel;
        // SAFETY: only copied frame values, on the app's UI thread.
        unsafe { mp_spectrum_panel(Rect { x, y, w, h }, radius, clip) };
        z.native.panel = Some(panel);
    }
    let duration = z.native.interpolated_interval(&z.timer, look.style);
    if look.style != Style::Bars {
        z.draw.tick(dt);
        z.draw.draw_native(
            &mut z.native.paths,
            look,
            stage,
            true,
            z.analyzer.levels(),
            z.analyzer.peaks(),
        );
        if changed {
            sync_paints(&g, stage, look, top, reflection, app.window().scale_factor());
        }
        if look.style.around() {
            z.native.sync_cover(app, stage, at);
        }
        present_paths(&z.native.paths, look.style, look.cover_pulse, duration);
        g.set_native_underlay(true);
        return true;
    }
    if changed {
        gradient(false, g.get_fill_brush());
        gradient(true, g.get_refl_brush0());
        let across = look.colour_by == ColourBy::Frequency && look.colours != Colours::Accent && reflection > 0.0;
        let alpha: [f32; draw::STRIPS] = std::array::from_fn(|i| {
            draw::fade((i as f32 + 0.5) / draw::STRIPS as f32) / draw::fade(0.5 / draw::STRIPS as f32)
        });
        // SAFETY: the static gradient copies its strip alphas before returning.
        unsafe { mp_spectrum_slices(alpha.as_ptr(), if across { alpha.len() } else { 0 }) };
    }
    z.draw.drew_native_bars();
    z.native.bars.clear();
    z.native.bars.extend(
        draw::bar_geometry(look, stage, top, reflection, z.analyzer.levels(), z.analyzer.peaks()).map(|b| Bar {
            x: b.x,
            width: b.width,
            height: b.height,
            radius: b.radius,
            cap: b.cap_y.unwrap_or(-1.0),
            reflection: b.reflection_height,
        }),
    );
    // SAFETY: the presenter copies the bar geometry into its shape paths before returning.
    unsafe { mp_spectrum_bars(z.native.bars.as_ptr(), z.native.bars.len(), top, duration) };
    g.set_native_underlay(true);
    true
}

// The order is the stage's paint order in vizstage.slint, also used in spectrum.m.
// The six reflection slices occupy indices 5 through 10.
const BLOOM: usize = 0;
const FILL: usize = 1;
const GLOW: usize = 2;
const LINE: usize = 3;
const CAPS: usize = 4;
const REFLECTION: usize = 5;
const RIPPLES: usize = 11;
const SPARKS_GLOW: usize = 12;
const SPARKS_OLD: usize = 13;
const SPARKS: usize = 14;
const FRONT_GLOW: usize = 15;
const FRONT: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl From<StageBounds> for Rect {
    fn from(frame: StageBounds) -> Self {
        Self { x: frame.x, y: frame.y, w: frame.width, h: frame.height }
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct Glass {
    blur: f32,
    left: f32,
    right: f32,
    bar: f32,
    pill: Rect,
    menu: Rect,
    submenu: Rect,
    menu_radius: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Slice {
    ptr: *const draw::PathCommand,
    len: usize,
}

impl From<&Vec<draw::PathCommand>> for Slice {
    fn from(commands: &Vec<draw::PathCommand>) -> Self {
        Self {
            ptr: commands.as_ptr(),
            len: commands.len(),
        }
    }
}

unsafe extern "C" {
    fn mp_spectrum_clear_geometry();
    fn mp_spectrum_glass(panes: Glass);
    fn mp_spectrum_panel(frame: Rect, radius: f32, clip: bool);
    fn mp_spectrum_paint(
        index: u32,
        kind: u32,
        rect: Rect,
        axis: Rect,
        stroke: f32,
        scale: f32,
        stops: *const Stop,
        count: usize,
    );
    fn mp_spectrum_paths(paths: *const Slice, pulse: f32, punch: f32, ring: bool, duration: f64, stationary_rim: bool);
    fn mp_spectrum_cover(rgb: *const u8, width: u32, height: u32, x: f32, y: f32, size: f32, radius: f32, behind: bool);
}

impl Presenter {
    fn sync_cover(&mut self, app: &AppWindow, stage: &Stage, at: Where) {
        let image = app.global::<Player>().get_full();
        let size = stage.hub * 2.0;
        let radius = if stage.corner < 0.0 { stage.hub } else { stage.corner };
        let key = (image, [stage.w / 2.0, stage.h / 2.0, size, radius], at == Where::Behind);
        if self.cover.as_ref() == Some(&key) {
            return;
        }
        if let Some(rgb) = key.0.to_rgb8() {
            let [x, y, size, radius] = key.1;
            // SAFETY: the native cover copies its RGB source before returning.
            unsafe {
                mp_spectrum_cover(
                    rgb.as_bytes().as_ptr(),
                    rgb.width(),
                    rgb.height(),
                    x,
                    y,
                    size,
                    radius,
                    key.2,
                )
            };
        }
        self.cover = Some(key);
    }
}

fn present_paths(paths: &draw::NativePaths, style: Style, cover_pulse: bool, duration: f64) {
    let mut layers = [Slice {
        ptr: std::ptr::null(),
        len: 0,
    }; FRONT + 1];
    for (index, commands) in [
        (FILL, &paths.fill),
        (GLOW, &paths.line),
        (LINE, &paths.line),
        (CAPS, &paths.caps),
        (RIPPLES, &paths.ripples),
        (SPARKS_GLOW, &paths.sparks_glow),
        (SPARKS_OLD, &paths.sparks_old),
        (SPARKS, &paths.sparks),
        (FRONT_GLOW, &paths.front_glow),
        (FRONT, &paths.front),
    ] {
        layers[index] = commands.into();
    }
    for (i, strip) in paths.refl.iter().enumerate() {
        layers[REFLECTION + i] = strip.into();
    }
    let pulse = 0.4 + 0.6 * paths.pulse.clamp(0.0, 1.0);
    let punch = if cover_pulse {
        1.0 + super::COVER_PULSE * paths.punch.clamp(0.0, 1.0)
    } else {
        1.0
    };
    // SAFETY: the frame's command slices remain valid while Core Animation builds the paths.
    unsafe {
        mp_spectrum_paths(
            layers.as_ptr(),
            pulse,
            punch,
            style.around(),
            duration,
            matches!(style, Style::Galaxy | Style::Saturn | Style::Swarm),
        )
    };
}

fn sync_paints(g: &Visualizer, stage: &Stage, look: &Look, top: f32, reflection: f32, scale: f32) {
    let overflow = if look.style.around() {
        let halo = draw::halo(stage);
        (halo.hub + halo.gap + halo.travel + halo.glow - stage.h.min(stage.w) / 2.0).max(0.0)
    } else {
        0.0
    };
    let bounds = Rect {
        x: -overflow,
        y: -overflow,
        w: stage.w + overflow * 2.0,
        h: top + overflow * 2.0,
    };
    for (index, brush, stroke) in [
        (BLOOM, g.get_bloom_brush(), 0.0),
        (FILL, g.get_fill_brush(), 0.0),
        (GLOW, g.get_glow_brush(), g.get_glow_width()),
        (LINE, g.get_line_brush(), g.get_line_width()),
        (CAPS, g.get_caps_brush(), 0.0),
        (RIPPLES, g.get_ripples_brush(), g.get_ripples_width()),
        (SPARKS_GLOW, g.get_sparks_glow_brush(), 0.0),
        (SPARKS_OLD, g.get_sparks_old_brush(), 0.0),
        (SPARKS, g.get_sparks_brush(), 0.0),
        (FRONT_GLOW, g.get_sparks_glow_brush(), 0.0),
        (FRONT, g.get_sparks_brush(), 0.0),
    ] {
        paint(index, brush, stroke, bounds, scale);
    }
    let strips = [
        g.get_refl_brush0(),
        g.get_refl_brush1(),
        g.get_refl_brush2(),
        g.get_refl_brush3(),
        g.get_refl_brush4(),
        g.get_refl_brush5(),
    ];
    for (i, brush) in strips.into_iter().enumerate() {
        paint(
            REFLECTION + i,
            brush,
            0.0,
            Rect {
                x: 0.0,
                y: top,
                w: stage.w,
                h: reflection,
            },
            scale,
        );
    }
}

fn paint(index: usize, brush: Brush, stroke: f32, bounds: Rect, scale: f32) {
    let mut axis = Rect {
        x: 0.5,
        y: 1.0,
        w: 0.5,
        h: 0.0,
    };
    let mut stops = Vec::new();
    let mut kind = 1;
    let stop = |s: &slint::private_unstable_api::re_exports::GradientStop| Stop {
        position: s.position,
        argb: s.color.as_argb_encoded(),
    };
    match brush {
        Brush::LinearGradient(g) => {
            if g.angle() == 90.0 {
                axis = Rect {
                    x: 0.0,
                    y: 0.5,
                    w: 1.0,
                    h: 0.5,
                };
            } else if g.angle() == 180.0 {
                axis.y = 0.0;
                axis.h = 1.0;
            }
            stops.extend(g.stops().map(stop));
        }
        Brush::RadialGradient(g) => {
            kind = 2;
            let (cx, cy) = g.center_or_default(bounds.w, bounds.h);
            let (rx, ry) = g.radii_or_default(bounds.w, bounds.h);
            axis = Rect {
                x: (cx - bounds.x) / bounds.w,
                y: (cy - bounds.y) / bounds.h,
                w: (cx - bounds.x + rx) / bounds.w,
                h: (cy - bounds.y + ry) / bounds.h,
            };
            stops.extend(g.stops().map(stop));
        }
        Brush::ConicGradient(g) => {
            kind = 3;
            let (cx, cy) = g.center_or_default(bounds.w, bounds.h);
            // Core Animation starts its sweep along startPoint→endPoint.
            axis = Rect {
                x: (cx - bounds.x) / bounds.w,
                y: (cy - bounds.y) / bounds.h,
                w: (cx - bounds.x) / bounds.w,
                h: (cy - bounds.y) / bounds.h - 0.5,
            };
            stops.extend(g.stops().map(stop));
        }
        _ => {}
    }
    // SAFETY: brushes and stops are copied into the attached scene before returning.
    unsafe {
        mp_spectrum_paint(
            index as u32,
            kind,
            bounds,
            axis,
            stroke,
            scale,
            stops.as_ptr(),
            stops.len(),
        )
    };
}

#[cfg(feature = "bench")]
pub(crate) fn snapshot(app: &AppWindow) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
    if !app.global::<Visualizer>().get_native_underlay() {
        return None;
    }
    unsafe extern "C" {
        fn mp_spectrum_snapshot(rgba: *mut u8, width: u32, height: u32) -> i32;
    }
    let size = app.window().size();
    let mut image = slint::SharedPixelBuffer::new(size.width, size.height);
    // SAFETY: the bitmap context writes exactly width × height RGBA pixels synchronously.
    match unsafe { mp_spectrum_snapshot(image.make_mut_bytes().as_mut_ptr(), size.width, size.height) } {
        1 => Some(image),
        2 => {
            let foreground = app.window().take_snapshot().ok()?;
            if foreground.width() != size.width || foreground.height() != size.height {
                return None;
            }
            // Skia supplies straight RGBA; the Core Graphics backdrop is premultiplied.
            let (back, _) = image.make_mut_bytes().as_chunks_mut::<4>();
            let (front, _) = foreground.as_bytes().as_chunks::<4>();
            for (out, front) in back.iter_mut().zip(front) {
                let alpha = u32::from(front[3]);
                let combined = alpha * 255 + u32::from(out[3]) * (255 - alpha);
                if let Some(weight) = std::num::NonZeroU32::new(combined) {
                    for c in 0..3 {
                        let color = u32::from(front[c]) * alpha + u32::from(out[c]) * (255 - alpha);
                        out[c] = ((color * 255 + combined / 2) / weight) as u8;
                    }
                }
                out[3] = ((combined + 127) / 255) as u8;
            }
            Some(image)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::look::{frame_interval, Fps};

    #[test]
    fn ripples_advance_on_each_requested_display_frame() {
        for fps in [Fps::Full, Fps::Half] {
            for refresh in [24, 50, 60, 120] {
                let interval = frame_interval(fps, Some(refresh));
                assert_eq!(model_frames(Style::Radial, interval), 1);
            }
        }
    }
}
