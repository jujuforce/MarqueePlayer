//! macOS: the window, with the system's frame and its traffic lights over the app's own
//! caption. The title bar is transparent and the content runs under it (winit's
//! `WindowAttributesExtMacOS`: a full-size content view, the title hidden), so the page
//! reaches the top edge as on Windows and Linux while the close, minimize and zoom buttons
//! are the system's, at the top left. The UI hides its own caption buttons and keeps that
//! corner free (`Chrome.native-caption`, `Chrome.caption-inset`, set in `install`).
//!
//! - **Moving**: the caption strip is Slint's `WindowMoveArea` (app.slint), which hands a
//!   drag to AppKit (`performWindowDragWithEvent:`); where the system's title bar takes the
//!   press itself, it moves the window the same way.
//! - **Double click on the caption**: what System Settings > Desktop & Dock says ("Double-click
//!   a window's title bar to": zoom, minimize, or nothing; `AppleActionOnDoubleClick`).
//! - **Resizing, the shadow, the rounded corners**: the system frame's.
//! - **The window menu**: macOS has none (a right click on the caption does nothing).
//!
//! Also here, as on Linux: the mouse side buttons (back, forward), files dropped from the
//! Finder (winit's drag and drop), the pin shortcuts taken before a text field sees them
//! (Command stands for Ctrl, as Slint makes it for every shortcut), and the app terminating
//! (Cmd+Q, the Dock's Quit, logging out: `Hooks::end_session`, as the session ending on
//! Windows). Everything runs in winit's event hook, before Slint, on the main thread.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;

use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::AppWindow;
use crate::platform::{FrameEvent, Hooks, NavInput, PinKey, PinSide};

pub use crate::platform::unix::virtual_key;

/// The traffic lights' room at the top left, logical pixels: the three buttons and their
/// margins (about 70 px), and a little air.
pub const TRAFFIC_LIGHTS_W: f32 = 78.0;

thread_local! {
    static APP: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
    static HOOKS: Cell<Option<Hooks>> = const { Cell::new(None) };
    static MAXIMIZED: Cell<Option<bool>> = const { Cell::new(None) };
    static MODS: Cell<winit::keyboard::ModifiersState> = const { Cell::new(winit::keyboard::ModifiersState::empty()) };
    static PIN_KEYS: RefCell<Vec<PinKey>> =
        RefCell::new(vec![(0x42, true, false, false, PinSide::Left), (0x4A, true, false, false, PinSide::Right)]);
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    /// Files dropped one event each, gathered for one hand-over.
    static DROPPED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// The window attributes every window gets: the system's frame, its title bar transparent
/// over a full-size content view, the title hidden (the app shows its own), the traffic
/// lights kept.
pub fn attributes(attrs: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    use winit::platform::macos::WindowAttributesExtMacOS;
    attrs
        .with_decorations(true)
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
        .with_title_hidden(true)
        .with_titlebar_buttons_hidden(false)
}

fn with_window<R>(f: impl FnOnce(&AppWindow, &winit::window::Window) -> R) -> Option<R> {
    let app = APP.with(|a| a.borrow().as_ref().and_then(slint::Weak::upgrade))?;
    app.window().with_winit_window(|w| f(&app, w))
}

fn content_view(window: &winit::window::Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(view) = window.window_handle().ok()?.as_raw() else { return None };
    Some(view.ns_view.as_ptr())
}

/// The live surface, when AppKit has attached it to the window.
fn window_surface(window: &winit::window::Window) -> Option<i32> {
    unsafe extern "C" {
        fn mp_macos_window_surface(view: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: winit keeps the NSView alive here, and this callback runs on its UI thread.
    Some(unsafe { mp_macos_window_surface(content_view(window)?) })
}

#[derive(PartialEq)]
struct ClockText {
    elapsed: slint::SharedString,
    family: slint::SharedString,
    ink: slint::Color,
    font_size: f32,
    scale: f32,
    size: slint::PhysicalSize,
}

#[derive(Default)]
struct ClockRaster {
    image: Option<(ClockText, slint::SharedPixelBuffer<slint::Rgba8Pixel>)>,
    revision: u64,
}

impl ClockRaster {
    fn render(&mut self, paint: &crate::ProgressPaint, scale: f32) -> Option<(&slint::SharedPixelBuffer<slint::Rgba8Pixel>, u64)> {
        let size = slint::PhysicalSize::new(
            (paint.clock.width * scale).round() as u32,
            (paint.clock.height * scale).round() as u32,
        );
        if paint.elapsed.is_empty() || size.width == 0 || size.height == 0 {
            return None;
        }
        let text = ClockText {
            elapsed: paint.elapsed.clone(), family: paint.family.clone(), ink: paint.dim,
            font_size: paint.font_size, scale, size,
        };
        if self.image.as_ref().is_none_or(|(old, _)| *old != text) {
            // winit creates even hidden native windows on the next macOS event-loop turn.
            // Drop the text component after its CPU snapshot. Only the small bitmap stays.
            let stamp = crate::ClockStamp::new().ok()?;
            let window = stamp.window();
            window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: scale });
            window.set_size(size);
            stamp.set_elapsed(text.elapsed.clone());
            stamp.set_family(text.family.clone());
            stamp.set_type_size(text.font_size);
            stamp.set_ink(text.ink);
            self.image = Some((text, window.take_snapshot().ok()?));
            self.revision += 1;
        }
        self.image.as_ref().map(|(_, pixels)| (pixels, self.revision))
    }
}

/// Present the steady transport on the Metal window, or let Slint paint it if unavailable.
fn paint_progress(clock: &mut ClockRaster, paint: crate::ProgressPaint) -> bool {
    unsafe extern "C" {
        fn mp_transport_paint(
            view: *mut std::ffi::c_void, scale: f32, width: u32,
            fill: *const [f32; 4], clock: *const [f32; 4],
            rgba: *const u8, w: u32, h: u32, revision: u64, ink: u32,
        ) -> bool;
    }
    let rect = |r: crate::StageBounds| [r.x, r.y, r.width, r.height];
    with_window(|app, window| {
        let Some(view) = content_view(window) else { return false };
        let scale = app.window().scale_factor();
        let (rgba, w, h, revision) = clock.render(&paint, scale).map_or(
            (std::ptr::null(), 0, 0, 0),
            |(pixels, revision)| (pixels.as_bytes().as_ptr(), pixels.width(), pixels.height(), revision),
        );
        // SAFETY: the NSView and bitmap are valid for the call, on their UI thread.
        unsafe {
            mp_transport_paint(
                view, scale, app.window().size().width, &rect(paint.fill), &rect(paint.clock),
                rgba, w, h, revision, paint.ink.as_argb_encoded(),
            )
        }
    }).unwrap_or(false)
}

/// The renderer installed in the current window.
pub fn renderer() -> Option<&'static str> {
    with_window(|_, window| match window_surface(window)? {
        1 => Some(super::system::TILED_RENDERER),
        2 => Some("Metal GPU window surface"),
        _ => None,
    }).flatten()
}

/// Once the winit window exists: keep the hooks, tell the UI the caption is the system's,
/// report the zoomed state, follow the app terminating. The handle is 0 (macOS code finds the
/// window through the app).
pub fn install(app: &AppWindow, hooks: Hooks) -> Option<isize> {
    let (maximized, surface) = app.window().with_winit_window(|w| (w.is_maximized(), window_surface(w)))?;
    let mut clock = ClockRaster::default();
    app.on_paint_progress(move |paint| paint_progress(&mut clock, paint));
    app.global::<crate::Visualizer>().set_metal_surface(surface == Some(2));
    APP.with(|a| *a.borrow_mut() = Some(app.as_weak()));
    HOOKS.with(|h| h.set(Some(hooks)));
    MAXIMIZED.with(|m| m.set(Some(maximized)));
    let chrome = app.global::<crate::Chrome>();
    chrome.set_native_caption(true);
    chrome.set_caption_inset(TRAFFIC_LIGHTS_W);
    (hooks.frame)(FrameEvent::Maximized(maximized));
    on_terminate(hooks.end_session);
    super::instance::window_ready(hooks);
    Some(0)
}

/// AppKit is terminating the app (the app menu's Quit or Cmd+Q, the Dock's Quit, logging
/// out, shutting down): it ends the process right after, without returning from the event
/// loop, so the app saves now.
fn on_terminate(end_session: fn(bool)) {
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
        log::info!("the app is terminating (Quit): saving");
        end_session(false);
    });
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the name is AppKit's constant; the block runs on the posting (main) thread, no
    // object or queue is given, and the observer it returns is kept for the whole run.
    let observer =
        unsafe { center.addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSApplicationWillTerminateNotification), None, None, &block) };
    std::mem::forget(observer);
}

fn emit(e: FrameEvent) {
    if let Some(h) = HOOKS.with(Cell::get) {
        (h.frame)(e);
    }
}

/// Input this module takes before Slint does. True when it was taken (Slint does not see
/// it).
pub fn winit_event(_app: &AppWindow, window: &slint::Window, event: &WindowEvent) -> bool {
    match event {
        WindowEvent::Resized(_) => {
            if let Some(m) = window.with_winit_window(|w| w.is_maximized())
                && MAXIMIZED.with(|c| c.replace(Some(m))) != Some(m)
            {
                emit(FrameEvent::Maximized(m));
            }
            false
        }
        WindowEvent::ModifiersChanged(m) => {
            MODS.with(|c| c.set(m.state()));
            false
        }
        WindowEvent::KeyboardInput { event, .. } => pin_key(event),
        WindowEvent::MouseInput { state, button, .. } => match (button, state) {
            (MouseButton::Back, ElementState::Released) => nav("mouse button 4 (back) up", NavInput::Back),
            (MouseButton::Forward, ElementState::Released) => nav("mouse button 5 (forward) up", NavInput::Forward),
            (MouseButton::Back | MouseButton::Forward, ElementState::Pressed) => {
                if let Some(h) = HOOKS.with(Cell::get) {
                    (h.nav)(format!("{button:?} button down (acts on release)"), None);
                }
                true
            }
            _ => false,
        },
        WindowEvent::DroppedFile(path) => {
            dropped(path.clone());
            true
        }
        WindowEvent::HoveredFile(_) | WindowEvent::HoveredFileCancelled => true,
        _ => false,
    }
}

fn nav(what: &str, to: NavInput) -> bool {
    if let Some(h) = HOOKS.with(Cell::get) {
        (h.nav)(what.to_owned(), Some(to));
    }
    true
}

/// A pin key: the keymap's Ctrl is Command here (`ModifiersState::super_key`), as Slint maps
/// it for the app's other shortcuts; the Control key itself is no pin key's.
fn pin_key(event: &winit::event::KeyEvent) -> bool {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    if event.state != ElementState::Pressed || event.repeat || CAPTURING.with(Cell::get) {
        return false;
    }
    let Some(vk) = virtual_key(&event.key_without_modifiers()) else { return false };
    let m = MODS.with(Cell::get);
    if m.control_key() {
        return false;
    }
    let side = PIN_KEYS
        .with(|k| k.borrow().iter().find(|k| k.0 == vk && (k.1, k.2, k.3) == (m.super_key(), m.shift_key(), m.alt_key())).map(|k| k.4));
    let Some(side) = side else { return false };
    if let Some(h) = HOOKS.with(Cell::get) {
        // Out of the event hook: pinning touches app state and the UI.
        let pin = h.pin;
        let _ = slint::invoke_from_event_loop(move || pin(side));
    }
    true
}

/// A file dropped (winit sends one event per file): gathered for a moment, then handed over
/// together. The drop point is not known (winit does not say where), so a drop plays.
fn dropped(path: PathBuf) {
    let first = DROPPED.with(|d| {
        let mut d = d.borrow_mut();
        d.push(path);
        d.len() == 1
    });
    if first {
        slint::Timer::single_shot(std::time::Duration::from_millis(60), || {
            let paths = DROPPED.with(|d| std::mem::take(&mut *d.borrow_mut()));
            if let Some(h) = HOOKS.with(Cell::get) {
                log::info!("{} file(s) dropped on the window", paths.len());
                (h.files)(paths, None);
            }
        });
    }
}

/// The pin actions' keys the window takes before the focused control (`shortcuts`).
pub fn set_pin_keys(keys: Vec<PinKey>) {
    PIN_KEYS.with(|k| *k.borrow_mut() = keys);
}

/// Settings > Keys is waiting for a key: the window takes none of its own.
pub fn set_capturing(on: bool) {
    CAPTURING.with(|c| c.set(on));
}

pub fn minimize() {
    with_window(|app, _| app.window().set_minimized(true));
}

/// What a double click on a title bar does, by System Settings (`AppleActionOnDoubleClick`:
/// "Maximize", the default, zooms; "Minimize"; "None"). The app's maximize button is not
/// shown here, so this is the caption's double click.
pub fn toggle_maximize() {
    let action = objc2_foundation::NSUserDefaults::standardUserDefaults()
        .stringForKey(&objc2_foundation::NSString::from_str("AppleActionOnDoubleClick"))
        .map(|s| s.to_string());
    match action.as_deref() {
        Some("Minimize") => minimize(),
        Some("None") => {}
        _ => {
            with_window(|app, w| app.window().set_maximized(!w.is_maximized()));
        }
    }
}

/// The same path as the red button or Cmd+W would take: a close request, which hides the
/// window to the menu bar (or quits, by the setting).
pub fn close() {
    with_window(|app, _| app.window().dispatch_event(slint::platform::WindowEvent::CloseRequested));
}

/// macOS has no window menu.
pub fn system_menu(_x: f32, _y: f32) {}

/// Bring the window back for a second launch, the Finder, the Dock or the media controls'
/// Raise: show it (from the menu bar too), unminimize it, and make the app active.
pub fn bring_back(app: &AppWindow, _hwnd: isize) {
    let _ = app.show();
    let quiet = super::devwindow::enabled();
    app.window().with_winit_window(|w| {
        w.set_minimized(false);
        if !quiet {
            w.focus_window();
        }
    });
}

/// Bench: a drop as the window would get it (the files and a point, physical pixels).
#[cfg(feature = "bench")]
pub fn simulate_drop(_hwnd: isize, paths: &[PathBuf], x: i32, y: i32) -> usize {
    if let Some(h) = HOOKS.with(Cell::get) {
        let (paths, files) = (paths.to_vec(), h.files);
        let n = paths.len();
        let _ = slint::invoke_from_event_loop(move || files(paths, Some((x, y))));
        return n;
    }
    0
}
