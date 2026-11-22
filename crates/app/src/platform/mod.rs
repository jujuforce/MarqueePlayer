//! The platform seam: everything the app asks of the system it runs on, behind one
//! interface, with one implementation per system (`windows/`, `linux/`, `macos/`; what Linux
//! and macOS share is in `unix/`). Application code calls `crate::platform::...` and never
//! names a system; nothing outside this folder is `cfg(windows)`, `cfg(target_os = "linux")`
//! or `cfg(target_os = "macos")` except where the build itself differs.
//!
//! Each system's module provides the same submodules with the same items (the compiler
//! checks it, as every build uses them):
//!
//! - `window`: the window around the app's own caption (on macOS, the system's traffic
//!   lights over it). `attributes` (the winit
//!   attributes every window gets), `install` once the native window exists (the app-drawn
//!   caption's hit testing and resize edges, side buttons, the window-level pin keys, files
//!   dropped on the window, a second launch's request; returns the native handle, 0 where
//!   there is none), `winit_event` (input the system's module takes before Slint does),
//!   `minimize`, `toggle_maximize`, `close`, `system_menu`, `bring_back`, `set_pin_keys`,
//!   `set_capturing`.
//! - `instance`: one running app per data folder, and handing files to it (`acquire`,
//!   `Instance`, `Guard`).
//! - `placement`: the window's saved size, position and maximized state (`read`,
//!   `restore`, `before_show`).
//! - `devwindow`: `MP_WINDOW=secondary` for test runs (`init`, `enabled`, `attributes`,
//!   `maximize`, and with the bench `resize_client`).
//! - `media`: the desktop's media controls (Windows' media overlay, MPRIS on Linux, Now
//!   Playing on macOS):
//!   `Controls`, fed what plays and handing back `MediaEvent`s.
//! - `shell`: the file manager (Explorer, the desktop's, the Finder), the browser and the clipboard (`show_item`, `open_folder`,
//!   `open_url`, `copy_text`).
//! - `integration`: the editions and what an installer registers (`detect`, `volume_root`,
//!   "Start with Windows" (a LaunchAgent on macOS) as `autostart` / `set_autostart`, the Default apps page,
//!   `installed_running` for the installer's mutex and quit request).
//! - `secret`: a secret kept on disk (`seal`, `open`, `write_private`, `SCHEME`): DPAPI for
//!   the current user on Windows, a file of mode 0600 on Linux and macOS.
//! - `system`: facts about the system (`os_version`, `reduced_motion`, `light_panel`,
//!   `tray_icon_size`, `local_offset_secs`, `offset_at`, `local_date`, `cpu_time`, `tray_host`,
//!   `ui_font`, `THEME_FONT`, the credits of what only this system links) and the wording
//!   that names it (`words`).
//! - `tray`: the tray icon (the menu bar's status item on macOS; `Tray`: its picture, tooltip
//!   and menu, handing back `TrayEvent`s for its menu, clicks and wheel), the system's double-click time
//!   (`double_click_time`), and `release` for the crash path (Windows' wheel hook).
//!
//! Types shared by every implementation are here.

#[cfg(windows)]
#[path = "windows/mod.rs"]
mod imp;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "macos/mod.rs"]
mod imp;

#[cfg(unix)]
mod unix;

pub use imp::{devwindow, instance, integration, media, placement, secret, shell, system, tray, window};

/// The macOS visualizer draws behind the premultiplied Slint window layer.
pub fn configure_renderer(_name: &str) {
    #[cfg(target_os = "macos")]
    softbuffer::set_macos_premultiplied_layer(matches!(_name, "software" | "sw").then_some("MarqueeSlintForeground"));
}

/// On macOS selecting Skia requests Metal explicitly. The generic Skia
/// renderer can otherwise fall back to its software surface when GPU initialization fails.
pub fn explicit_renderer(selector: slint::BackendSelector, name: &str) -> slint::BackendSelector {
    #[cfg(target_os = "macos")]
    if matches!(name, "skia" | "winit-skia" | "winit-skia-wgpu") {
        return selector.backend_name("winit-skia".into()).require_metal();
    }
    selector.backend_name(name.to_owned())
}

pub fn window_renderer(selected: &'static str) -> &'static str {
    #[cfg(target_os = "macos")]
    if let Some(renderer) = imp::window::renderer() {
        return renderer;
    }
    selected
}

/// The app's id: the Linux desktop file's name, its icon's, the Wayland app id and X11
/// class, the PipeWire application id and the MPRIS bus name's last part, so the desktop
/// groups the window with its launcher and shows its icon everywhere; on macOS the
/// LaunchAgent's label (and the bundle identifier the packaging gives the app). Reverse-DNS,
/// as freedesktop and Apple ask.
pub const APP_ID: &str = "app.marqueeplayer.MarqueePlayer";

/// Caption button size, logical pixels. Three of them at the top right: minimize,
/// maximize/restore, close. chrome.slint draws them at the same geometry. (Not on macOS,
/// whose own traffic lights take their place.)
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const BUTTON_W: f32 = 46.0;
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const BUTTON_H: f32 = 32.0;

/// What the frame tells the UI. macOS sends `Maximized` only: its caption buttons are the
/// system's.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub enum FrameEvent {
    MaxHover(bool),
    MaxPressed(bool),
    Maximized(bool),
    /// The pointer is near the caption buttons (they show only then).
    CaptionZone(bool),
}

/// Where the caption buttons appear on hover: the top-right corner, a little wider and
/// taller than the buttons themselves. Client coordinates, physical pixels.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn in_caption_zone(x: f32, y: f32, width: f32, scale: f32) -> bool {
    y >= 0.0 && y < (BUTTON_H + 16.0) * scale && x >= width - (3.0 * BUTTON_W + 48.0) * scale && x < width
}

/// What a mouse side button or browser key means for navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavInput {
    Back,
    Forward,
}

/// Which sidebar a pin shortcut toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinSide {
    /// Ctrl+B by default: the navigation sidebar.
    Left,
    /// Ctrl+Q by default (Cmd+J on macOS, where Cmd+Q quits): the queue.
    Right,
}

/// A pin key the window takes: (Win32 virtual-key code, Ctrl, Shift, Alt, which side).
/// The keymap's bindings name keys by those codes on every system.
pub type PinKey = (u16, bool, bool, bool, PinSide);

/// Where the window's input goes, given to `frame::install`.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// A pin shortcut taken at the window (before a text field sees it).
    pub pin: fn(PinSide),
    /// A side button or browser key, with its description for the log.
    pub nav: fn(String, Option<NavInput>),
    /// The caption's state for the app-drawn buttons.
    pub frame: fn(FrameEvent),
    /// A second launch asked for the window (with the native handle).
    pub show: fn(isize),
    /// Files from outside: a second launch's (no point) or dropped on the window (the
    /// drop point, physical pixels in the window).
    pub files: fn(Vec<std::path::PathBuf>, Option<(i32, i32)>),
    /// The session is ending (shutdown, sign-out) or an installer's Restart Manager asked
    /// the app to close (`true`): save now, the process may not live past the call.
    /// (Windows; on macOS the app terminating, Cmd+Q or the Dock's Quit, with `false`;
    /// Linux has no such message to a window.)
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub end_session: fn(bool),
}

/// What the tray icon hands the app (`tray::Tray`): its menu's items, and the mouse on
/// the icon. The app decides what a click means (`crate::tray`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// The left button released over the icon (Windows), or the item activated (Linux).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Click,
    /// The system's double click on the icon (Windows: `WM_LBUTTONDBLCLK`). Where the
    /// system sends none (Linux), two clicks within the double-click time are one.
    #[cfg_attr(not(windows), allow(dead_code))]
    DoubleClick,
    /// The middle button (Windows), or the item's secondary activation (Linux).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Middle,
    /// The wheel over the icon: 120 a notch, positive away from the user.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Wheel(i32),
    TogglePlay,
    Next,
    Previous,
    ShowWindow,
    About,
    Quit,
}

/// The bench: what it hands the tray icon in place of the mouse (`tray::Tray::simulate`),
/// delivered inside the app as the system would deliver it; nothing reaches the real input.
#[cfg(feature = "bench")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayPointer {
    Click,
    DoubleClick,
    Middle,
    /// 120 a notch.
    Wheel(i32),
    /// The pointer moving over the icon (Windows: arms the wheel hook if it really is there).
    Hover,
}

/// What the desktop's media controls asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaEvent {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    /// Seconds, either way.
    SeekBy(f64),
    /// Seconds into the track.
    SetPosition(f64),
    Shuffle(bool),
    /// Off, all, one: `crate::settings` repeat order.
    Repeat(u8),
    /// 0 to 1.
    Volume(f64),
    /// Show the window.
    Raise,
    Quit,
}

/// The words that name the system in the UI (`system::words`), given to the `Platform`
/// global at start and used where Rust builds text.
#[derive(Clone, Copy, Debug)]
pub struct Words {
    /// About's line under the name.
    pub tagline: &'static str,
    /// The menus' entry: "Show in Explorer".
    pub show_in: &'static str,
    /// View metadata's labels for it, for a file and for an album's folder.
    pub show_file: &'static str,
    pub show_folder: &'static str,
    /// "Open the folder of the <what> in Explorer": the part after "<what>".
    pub open_folder_in: &'static str,
    /// About's Copy: what the line names ("Windows"), and its button's label.
    pub copy_names: &'static str,
    pub copy_label: &'static str,
    /// Settings > Output, the line under "System default".
    pub default_output: &'static str,
    /// The exclusive setting's name, hint and "More".
    pub exclusive_title: &'static str,
    pub exclusive_hint: &'static str,
    pub exclusive_more: &'static str,
    /// Where a standalone copy keeps its files: "user profile", "home folder".
    pub profile: &'static str,
    /// Settings > Window, the setting that starts the app with the session: "Start with
    /// Windows", "Open at login".
    pub autostart: &'static str,
    /// Its "More": where the system's own settings show it.
    pub autostart_more: &'static str,
}

/// What the media controls show of the playing state, beyond the track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaOptions {
    pub shuffle: bool,
    /// Off, all, one.
    pub repeat: u8,
    pub volume: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_zone_is_the_top_right_corner() {
        assert!(in_caption_zone(1430.0, 5.0, 1440.0, 1.0));
        assert!(in_caption_zone(1260.0, 40.0, 1440.0, 1.0));
        assert!(!in_caption_zone(1200.0, 5.0, 1440.0, 1.0));
        assert!(!in_caption_zone(1430.0, 60.0, 1440.0, 1.0));
    }

    #[test]
    fn the_app_id_is_reverse_dns() {
        assert_eq!(APP_ID.split('.').count(), 3);
        assert!(APP_ID.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
    }

    /// The Linux desktop file, metainfo and icons name the app as the code does (the
    /// window's app id, MPRIS's DesktopEntry, PipeWire's application id), so the desktop
    /// matches them; and every format the scanner takes has a MIME type in the desktop file.
    #[test]
    fn the_linux_packaging_files_use_the_app_id() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |p: std::path::PathBuf| std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let dir = root.join("../../packaging/linux");
        let desktop = read(dir.join(format!("{APP_ID}.desktop")));
        assert!(desktop.contains(&format!("\nStartupWMClass={APP_ID}\n")), "{desktop}");
        assert!(desktop.contains(&format!("\nIcon={APP_ID}\n")));
        let meta = read(dir.join(format!("{APP_ID}.metainfo.xml")));
        assert!(meta.contains(&format!("<id>{APP_ID}</id>")) && meta.contains(&format!("{APP_ID}.desktop</launchable>")));
        assert!(read(root.join("examples/linux_icons.rs")).contains(&format!("\"{APP_ID}\"")));
        let mime = |ext: &str| -> &[&str] {
            match ext {
                "opus" => &["audio/opus", "audio/x-opus+ogg"],
                "ogg" | "oga" => &["audio/ogg", "audio/x-vorbis+ogg"],
                "flac" => &["audio/flac", "audio/x-flac"],
                "mp3" => &["audio/mpeg"],
                "mp2" => &["audio/mpeg", "audio/mp2"],
                "wav" => &["audio/x-wav", "audio/wav"],
                "aif" | "aiff" | "aifc" => &["audio/x-aiff", "audio/x-aifc"],
                "aac" => &["audio/aac", "audio/x-aac"],
                "m4a" | "mp4" => &["audio/mp4", "audio/x-m4a"],
                "m4b" => &["audio/x-m4b"],
                other => panic!("a scanned extension with no MIME type here: {other}"),
            }
        };
        let line = desktop.lines().find_map(|l| l.strip_prefix("MimeType=")).unwrap();
        let listed: Vec<&str> = line.split(';').collect();
        for ext in library::scan::AUDIO_EXTS {
            for m in mime(ext) {
                assert!(listed.contains(m), "{ext}: {m} missing from MimeType");
            }
        }
    }
}
