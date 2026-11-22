//! macOS: facts about the system, read where the app needs them. The version from
//! `NSProcessInfo`, the appearance and the double-click action from the user's global
//! defaults, reduced motion from `NSWorkspace`'s accessibility settings, times from the C
//! library's time zone rules (`localtime_r`, which follows System Settings' time zone).

use std::sync::OnceLock;
use std::time::Duration;

/// Where the app keeps its files (`paths`), as Apple's guidelines put an app's: settings,
/// the library and the log in `~/Library/Application Support/MarqueePlayer`, the caches
/// (safe to delete) in `~/Library/Caches/MarqueePlayer`.
pub fn app_dirs() -> crate::paths::Dirs {
    use std::path::PathBuf;
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| PathBuf::from("/tmp"));
    let support = home.join("Library/Application Support/MarqueePlayer");
    crate::paths::Dirs { config: support.clone(), data: support.clone(), state: support, cache: home.join("Library/Caches/MarqueePlayer") }
}

/// The theme's default font (theme.toml, `[type] font_family`): empty, the system's UI font
/// (`ui_font`).
pub const THEME_FONT: &str = "\"\"";

/// Skia's OpenGL surface is not in the macOS build (Apple deprecated OpenGL).
pub const HAS_SKIA_OPENGL: bool = false;

/// Core Animation composites Slint's retained software tiles and the native visualizer.
pub const TILED_RENDERER: &str = "Core Animation compositor, software-rendered UI tiles";
pub const RENDERERS: &[(&str, &str)] = &[("skia", "Skia (Metal)"), ("software", TILED_RENDERER)];

/// A headless Mac or a machine without a Metal device can still open the tiled player.
pub fn has_metal_device() -> bool {
    unsafe extern "C" { fn mp_macos_has_metal_device() -> bool; }
    // SAFETY: the function checks Metal's default device without keeping it.
    unsafe { mp_macos_has_metal_device() }
}

/// The menu bar's status item is always the static glyph: each frame of the moving one went
/// through Slint's AppKit tray as a PNG encoded and decoded again, then a menu bar redraw, five
/// times a second for as long as music played (`tray::anim`). Settings does not offer it.
pub const TRAY_ANIMATES: bool = false;

/// The theme's family as it can be drawn here: as named (Core Text finds a fallback).
pub fn font_here(family: &str) -> String {
    family.to_owned()
}

/// The system's UI font, San Francisco, by the name the font collection gives it on macOS
/// ("System Font"; its files are hidden from the font list under other names).
pub fn ui_font() -> String {
    "System Font".to_owned()
}

/// A string from the user's global defaults (`defaults read -g <key>`).
fn global_default(key: &str) -> Option<String> {
    objc2_foundation::NSUserDefaults::standardUserDefaults().stringForKey(&objc2_foundation::NSString::from_str(key)).map(|s| s.to_string())
}

/// "macOS 15.1.1 (Apple silicon)", read once.
pub fn os_version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        let patch = if v.patchVersion > 0 { format!(".{}", v.patchVersion) } else { String::new() };
        let chip = if cfg!(target_arch = "aarch64") { "Apple silicon" } else { "Intel" };
        format!("macOS {}.{}{patch} ({chip})", v.majorVersion, v.minorVersion)
    })
}

/// System Settings > Accessibility > Display > Reduce motion.
pub fn reduced_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// The menu bar is light: the system's appearance is not Dark (`AppleInterfaceStyle` is set
/// only in Dark mode, and in Auto while it is dark), for the status item glyph's deeper colours.
pub fn light_panel() -> bool {
    global_default("AppleInterfaceStyle").is_none_or(|s| !s.eq_ignore_ascii_case("dark"))
}

/// The status item's icon size: the menu bar's icons are 16 to 18 points; drawn at twice
/// that for a Retina display, which AppKit scales down.
pub fn tray_icon_size() -> u32 {
    36
}

/// The menu bar is always there to hide the window to.
pub fn tray_host() -> bool {
    true
}

fn local_tm(unix_secs: i64) -> Option<libc::tm> {
    let t = unix_secs as libc::time_t;
    // SAFETY: a plain struct for localtime_r to fill; `t` outlives the call.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: as above (localtime_r is the thread-safe form).
    (!unsafe { libc::localtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

/// The local time's offset from UTC now, in seconds (daylight saving included).
pub fn local_offset_secs() -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    local_tm(now).map_or(0, |tm| tm.tm_gmtoff)
}

/// The local time's offset from UTC at a Unix time, in seconds (the zone's rules for that
/// instant, daylight saving included). The offset now when the conversion fails.
pub fn offset_at(unix_secs: i64) -> i64 {
    local_tm(unix_secs).map_or_else(local_offset_secs, |tm| tm.tm_gmtoff)
}

/// The local date of a Unix time.
pub fn local_date(unix_secs: i64) -> Option<(i64, u32, u32)> {
    local_tm(unix_secs).map(|tm| (i64::from(tm.tm_year) + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32))
}

/// This process's CPU time (user + system), for the bench.
#[cfg_attr(not(feature = "bench"), allow(dead_code))]
pub fn cpu_time() -> Duration {
    // SAFETY: a plain struct for getrusage to fill.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
        return Duration::ZERO;
    }
    let tv = |t: libc::timeval| Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64);
    tv(u.ru_utime) + tv(u.ru_stime)
}

/// macOS counts CPU time precisely (`cpu_time`); no cycle counter per process here.
#[cfg(feature = "bench")]
pub fn cycles() -> Option<u64> {
    None
}

/// GDI and USER objects are Windows'; nothing to count here.
#[cfg(feature = "bench")]
pub fn gui_objects() -> (u32, u32) {
    (0, 0)
}

/// Current and peak physical footprint in MiB (including Metal allocations), and open file descriptors.
#[cfg(feature = "bench")]
pub fn resources() -> (f64, f64, u32) {
    let fds = std::fs::read_dir("/dev/fd").map(|d| d.count().saturating_sub(1) as u32).unwrap_or(0);
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: proc_pid_rusage writes a v4 record on success, despite its opaque buffer type.
    unsafe {
        if libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, usage.as_mut_ptr().cast()) != 0 {
            return (0.0, 0.0, fds);
        }
        let usage = usage.assume_init();
        (usage.ri_phys_footprint as f64 / 1_048_576.0, usage.ri_lifetime_max_phys_footprint as f64 / 1_048_576.0, fds)
    }
}

/// About's credits for what only the macOS build links (`about::credits`).
pub const CREDITS_SOUND: &[crate::about::Credit] = &[crate::about::Credit {
    name: "objc2-audio-toolbox",
    what: "Sound output through Core Audio",
    licence: "Zlib, Apache-2.0 or MIT",
    package: "objc2-audio-toolbox",
    package_licence: "Zlib OR Apache-2.0 OR MIT",
}];
pub const CREDITS_DESKTOP: &[crate::about::Credit] = &[crate::about::Credit {
    name: "souvlaki",
    what: "Media keys and Now Playing",
    licence: "MIT",
    package: "souvlaki",
    package_licence: "MIT",
}];
pub const CREDITS_SYSTEM: &[crate::about::Credit] = &[
    crate::about::Credit { name: "objc2", what: "AppKit and Foundation bindings", licence: "MIT", package: "objc2", package_licence: "MIT" },
    crate::about::Credit { name: "libc", what: "Unix API bindings", licence: "MIT or Apache-2.0", package: "libc", package_licence: "MIT OR Apache-2.0" },
];
/// What wgpu drives under Skia here.
pub const GPU_API_UNDER_SKIA: &str = "Metal under Skia";
/// The target About's credits test checks the dependencies of.
#[cfg_attr(not(test), allow(dead_code))]
pub const TARGET_TRIPLE: &str = "aarch64-apple-darwin";

/// The words that name the system in the UI.
pub const WORDS: crate::platform::Words = crate::platform::Words {
    tagline: "A local-first, artwork-forward music player for macOS.",
    show_in: "Show in Finder",
    show_file: "Show the file in the Finder",
    show_folder: "Show the album's folder in the Finder",
    open_folder_in: " in the Finder",
    copy_names: "macOS",
    copy_label: "Copy the version, macOS version and renderer, for a bug report",
    default_output: "The system default follows the output chosen in System Settings > Sound, also when it changes.",
    exclusive_title: "Exclusive mode",
    exclusive_hint: "Not available on macOS: playback shares the device with other apps, through Core Audio.",
    exclusive_more: "Core Audio plays at the device's own rate (Audio MIDI Setup sets it) and converts each track to it. Volume, ReplayGain and the equalizer apply in the app; the technical details say the rate the device runs at.",
    profile: "home folder",
    autostart: "Open at login",
    autostart_more: "A LaunchAgent in your Library (LaunchAgents) starts it. System Settings > General > Login Items lists it as a background item; turned off there, it does not start at login.",
};

pub fn words() -> crate::platform::Words {
    WORDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dates_follow_the_time_zone() {
        // 2026-07-01 12:00 UTC is that day in every zone of the world.
        assert_eq!(local_date(1_782_907_200), Some((2026, 7, 1)));
        assert!(local_offset_secs().abs() <= 14 * 3600);
    }

    #[test]
    fn the_system_line_names_macos() {
        assert!(os_version().starts_with("macOS "), "{}", os_version());
    }

    #[test]
    fn the_files_are_in_the_library() {
        let d = app_dirs();
        assert!(d.data.ends_with("Library/Application Support/MarqueePlayer"));
        assert!(d.cache.ends_with("Library/Caches/MarqueePlayer"));
    }
}
