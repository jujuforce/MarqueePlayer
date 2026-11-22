//! Settings > About: the version (and a line to copy into a bug report), where the app
//! keeps its files, the library at a glance, and the open-source components it is built on.
//!
//! The version is `APP_VERSION` from `build.rs`: `YYYY.M.D.count` for a release made by
//! `cargo xtask release`, `YYYY.M.D-dev` for any other build.
//!
//! Nothing here goes online. Sizes and counts are measured each time the section is shown,
//! on a thread of their own (a read-only database connection, a walk of the two caches),
//! and that work stops as soon as the section or the page is left; nothing runs otherwise.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::paths::Paths;
use crate::state::{self, State};
use crate::{AboutCredit, AboutFile, AboutView};

pub const NAME: &str = "Marquee Player";
/// Shown in About and in the exe's and the installer's properties (build.rs, the installer
/// script), and at the top of LICENSE.txt.
pub const COPYRIGHT: &str = "Copyright \u{a9} 2026 Offbeat";
/// The terms, under the copyright line in About (as LICENSE.txt and LICENSE state them): the
/// GPL's "Appropriate Legal Notices" (copyright, no warranty, where to read the licence).
pub const LICENCE: &str = "Free software under the GNU General Public License, version 3 or later, with no warranty. LICENSE.txt, in the download, has the licence in full.";
/// Where the source code is published: `repository` in the workspace's Cargo.toml, the one
/// place it is set; empty until it is.
pub const SOURCE: &str = env!("CARGO_PKG_REPOSITORY");

/// The lines under the name in About: copyright, licence and, once it is set, the source.
pub fn legal() -> String {
    match SOURCE {
        "" => format!("{COPYRIGHT}. {LICENCE}"),
        url => format!("{COPYRIGHT}. {LICENCE}\nSource code: {url}"),
    }
}
/// What the app is, in About (the system named as `platform::Words::tagline` says).
pub fn description() -> &'static str {
    crate::platform::system::words().tagline
}

/// "2026.11.15.3" for a release, "2026.11.15-dev" for any other build (build.rs).
pub const VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// A build that `cargo xtask release` did not number.
pub fn is_dev() -> bool {
    VERSION.ends_with("-dev") || VERSION == "dev"
}

/// The client name sent to Last.fm.
pub fn agent() -> String {
    format!("MarqueePlayer/{VERSION}")
}

/// The client name sent to LRCLIB, which asks clients to say what they are.
pub fn lyrics_agent() -> String {
    format!("MarqueePlayer/{VERSION} (desktop music player)")
}

// ---- what the app runs on -------------------------------------------------------------

static RENDERER: OnceLock<String> = OnceLock::new();

/// The renderer the window got (main.rs, `select_renderer`), for the report line.
pub fn set_renderer(name: &str) {
    let _ = RENDERER.set(name.to_owned());
}

pub fn renderer() -> &'static str {
    crate::platform::window_renderer(RENDERER.get().map_or("renderer not chosen yet", String::as_str))
}
/// "Windows 11 24H2 (build 26100.2605)", "Ubuntu 24.04.1 LTS (Linux 6.8), KDE on Wayland":
/// what the app runs on, read once (`platform::system::os_version`).
pub fn os_version() -> &'static str {
    crate::platform::system::os_version()
}

/// The line "Copy" puts on the clipboard, for a bug report: name, version and edition,
/// Windows, renderer.
pub fn report_line(version: &str, edition: &str, os: &str, arch: &str, renderer: &str) -> String {
    format!("{NAME} {version} ({edition}) \u{b7} {os} \u{b7} {arch} \u{b7} {renderer}")
}

/// This run's report line.
pub fn this_report_line() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    report_line(VERSION, crate::edition::get().short(), os_version(), arch, renderer())
}

// ---- where the files are ----------------------------------------------------------------

/// One of the places the app writes to: a file (Open folder shows it selected in its
/// folder) or a folder (opened).
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub what: &'static str,
    pub path: PathBuf,
    pub folder: bool,
    /// What it is, before anything is measured.
    pub note: &'static str,
}

/// Every place, in the order About lists them.
pub fn places(p: &Paths) -> Vec<Place> {
    vec![
        Place { what: "Settings", path: p.settings(), folder: false, note: "Folders, playback, window, keys" },
        Place { what: "Theme", path: p.theme(), folder: false, note: "Colours and sizes; edit it and the window follows" },
        Place { what: "Library database", path: p.database(), folder: false, note: "Plays, favourites, playlists, the saved queue" },
        Place { what: "Art cache", path: p.art(), folder: true, note: "Safe to delete: the next scan makes it again" },
        Place { what: "Lyrics cache", path: p.lyrics(), folder: true, note: "Safe to delete: tracks are looked up again as they play" },
        Place { what: "Log", path: p.log(), folder: false, note: "This run's log; the previous run's is kept beside it as marquee-player.previous.log" },
    ]
}

pub const DATABASE: usize = 2;

/// What was measured: sizes in bytes (None: not there), the library's totals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measured {
    pub sizes: Vec<Option<(u64, u64)>>,
    pub albums: i64,
    pub tracks: i64,
    pub seconds: f64,
    pub bytes: i64,
    pub sources: library::remote::Counts,
}

/// A place's line under its path: what it is, and once measured, how big.
pub fn place_detail(i: usize, place: &Place, size: Option<Option<(u64, u64)>>) -> String {
    let size = match size {
        None => "Measuring\u{2026}".to_owned(),
        Some(None) if place.folder => "Empty: nothing kept yet".to_owned(),
        Some(None) => "Not written yet".to_owned(),
        Some(Some((bytes, files))) if place.folder => format!("{} in {} {}", crate::metadata::view::short_size(bytes), library::audio::thousands(files), if files == 1 { "file" } else { "files" }),
        Some(Some((bytes, _))) => crate::metadata::view::short_size(bytes),
    };
    let size = if i == DATABASE { format!("Schema {} \u{b7} {size}", library::db::SCHEMA_VERSION) } else { size };
    format!("{} \u{b7} {size}", place.note)
}

/// A path as shown: a zero-width space after each separator, so that a long path wraps at
/// a folder in a narrow column rather than wherever it can.
pub fn breakable_path(path: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    path.replace(sep, &format!("{sep}\u{200b}"))
}

/// The size of a file (with SQLite's write-ahead log beside it, for the database), or of
/// everything in a folder and how many files; None when it is not there. Stops early
/// (None) when `go_on` says so.
pub fn measure(path: &Path, folder: bool, go_on: &dyn Fn() -> bool) -> Option<(u64, u64)> {
    if !folder {
        let main = std::fs::metadata(path).ok()?.len();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::fs::metadata(PathBuf::from(wal)).map_or(0, |m| m.len());
        return Some((main + wal, 1));
    }
    if !path.is_dir() {
        return None;
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    for e in walkdir::WalkDir::new(path).into_iter().filter_map(Result::ok) {
        if files.is_multiple_of(256) && !go_on() {
            return None;
        }
        if e.file_type().is_file() {
            bytes += e.metadata().map_or(0, |m| m.len());
            files += 1;
        }
    }
    (files > 0).then_some((bytes, files))
}

/// The library's totals, from its own read-only connection.
fn library_totals(db: &Path) -> rusqlite::Result<(i64, i64, f64, i64, library::remote::Counts)> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let (albums, tracks, seconds, bytes) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM albums), COUNT(*), COALESCE(SUM(duration), 0), COALESCE(SUM(size), 0) FROM tracks",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    // Where the tracks are: the folders, the server, both (Navidrome).
    Ok((albums, tracks, seconds, bytes, library::remote::counts(&conn)?))
}

/// "3 days 4 h", "12 h 5 min", "42 min".
pub fn total_length(secs: f64) -> String {
    let mins = (secs.max(0.0) / 60.0).round() as u64;
    let (days, hours, mins) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    match (days, hours) {
        (0, 0) => format!("{mins} min"),
        (0, h) => format!("{h} h {mins} min"),
        (1, h) => format!("1 day {h} h"),
        (d, h) => format!("{} days {h} h", library::audio::thousands(d)),
    }
}

// ---- built with ---------------------------------------------------------------------------

/// The major open-source components, with what they do here and their licence as shown.
/// `package` and `package_licence` are the package and licence field the test checks
/// against `cargo metadata`, so the list cannot drift from the dependencies.
pub struct Credit {
    pub name: &'static str,
    pub what: &'static str,
    pub licence: &'static str,
    // Read by the test only.
    #[cfg_attr(not(test), allow(dead_code))]
    pub package: &'static str,
    #[cfg_attr(not(test), allow(dead_code))]
    pub package_licence: &'static str,
}

/// Every credit, in the order About lists them: the common ones with each system's own
/// in their place (`platform::system::CREDITS_SOUND`, `_DESKTOP`, `_SYSTEM`).
pub fn credits() -> impl Iterator<Item = &'static Credit> {
    use crate::platform::system::{CREDITS_DESKTOP, CREDITS_SOUND, CREDITS_SYSTEM};
    CREDITS_A.iter().chain(CREDITS_SOUND).chain(CREDITS_B).chain(CREDITS_DESKTOP).chain(CREDITS_C).chain(CREDITS_SYSTEM)
}

const CREDITS_A: &[Credit] = &[
    Credit {
        name: "Slint",
        what: "The user interface toolkit",
        licence: "GPL-3.0 (one of its three licences)",
        package: "slint",
        package_licence: "GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0",
    },
    Credit { name: "Skia, through rust-skia", what: "Drawing the window", licence: "BSD-3-Clause (Skia), MIT (rust-skia)", package: "skia-safe", package_licence: "MIT" },
    Credit { name: "wgpu", what: crate::platform::system::GPU_API_UNDER_SKIA, licence: "MIT or Apache-2.0", package: "wgpu", package_licence: "MIT OR Apache-2.0" },
    Credit { name: "winit", what: "The window and its input", licence: "Apache-2.0", package: "winit", package_licence: "Apache-2.0" },
    Credit { name: "AccessKit", what: "Screen reader support", licence: "MIT or Apache-2.0", package: "accesskit", package_licence: "MIT OR Apache-2.0" },
    Credit {
        name: "Symphonia",
        what: "Reading and decoding FLAC, MP3, AAC, ALAC, Vorbis, WAV and AIFF",
        licence: "MPL-2.0 (source at github.com/pdeljanov/Symphonia)",
        package: "symphonia",
        package_licence: "MPL-2.0",
    },
    Credit { name: "libopus, through opusic-sys", what: "Decoding Opus", licence: "BSD-3-Clause", package: "opusic-sys", package_licence: "BSD-3-Clause" },
];

const CREDITS_B: &[Credit] = &[
    Credit { name: "Lofty", what: "Reading tags", licence: "MIT or Apache-2.0", package: "lofty", package_licence: "MIT OR Apache-2.0" },
    Credit { name: "SQLite, through rusqlite", what: "The library database", licence: "Public domain (SQLite), MIT (rusqlite)", package: "rusqlite", package_licence: "MIT" },
    Credit { name: "image", what: "Cover art: JPEG, PNG and WebP", licence: "MIT or Apache-2.0", package: "image", package_licence: "MIT OR Apache-2.0" },
];

const CREDITS_C: &[Credit] = &[
    Credit { name: "rfd", what: "The folder and file dialogs", licence: "MIT", package: "rfd", package_licence: "MIT" },
    Credit { name: "serde, serde_json, toml", what: "Settings and theme files", licence: "MIT or Apache-2.0", package: "serde_json", package_licence: "MIT OR Apache-2.0" },
];

/// Under the list: everything else.
pub const CREDITS_NOTE: &str = "And about 300 smaller Rust crates, each under its own licence (MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, BSL-1.0, Unicode). THIRD-PARTY-NOTICES.txt, beside the exe in the release download, lists every one with its licence text.";

// ---- the section ---------------------------------------------------------------------------

/// Bumped when About is shown or left: a measurement for an older one stops and its
/// results are dropped.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The section was left (another section, another page): stop measuring.
pub fn leave() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// What never changes during a run: the version, the paths, the credits. Set at start.
pub fn install(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let v = app.global::<AboutView>();
    v.set_icon(crate::tray::icon::app_icon(128));
    v.set_name(NAME.into());
    v.set_description(description().into());
    v.set_copyright(legal().into());
    v.set_edition(crate::edition::get().describe().into());
    v.set_version(VERSION.into());
    v.set_dev(is_dev());
    let credits: Vec<AboutCredit> = credits().map(|c| AboutCredit { name: c.name.into(), what: c.what.into(), licence: c.licence.into() }).collect();
    v.set_credits(ModelRc::new(VecModel::from(credits)));
    v.set_credits_note(CREDITS_NOTE.into());
    show_files(st, None);
}

/// The file rows, with sizes once measured.
fn show_files(st: &Rc<State>, m: Option<&Measured>) {
    let Some(app) = st.app.upgrade() else { return };
    let rows: Vec<AboutFile> = places(&st.paths)
        .iter()
        .enumerate()
        .map(|(i, p)| AboutFile {
            what: p.what.into(),
            path: breakable_path(&p.path.display().to_string()).into(),
            detail: place_detail(i, p, m.map(|m| m.sizes.get(i).copied().flatten())).into(),
        })
        .collect();
    app.global::<AboutView>().set_files(ModelRc::new(VecModel::from(rows)));
}

/// About was shown: the system line, then sizes and counts measured off the UI thread.
pub fn refresh(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let v = app.global::<AboutView>();
    v.set_system(format!("{} \u{b7} {}", os_version(), renderer()).into());
    v.set_measuring(true);
    show_files(st, None);
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let paths = st.paths.clone();
    std::thread::Builder::new()
        .name("about".into())
        .spawn(move || {
            let go_on = || GENERATION.load(Ordering::Relaxed) == generation;
            let mut m = Measured::default();
            match library_totals(&paths.database()) {
                Ok((albums, tracks, seconds, bytes, sources)) => (m.albums, m.tracks, m.seconds, m.bytes, m.sources) = (albums, tracks, seconds, bytes, sources),
                Err(e) => log::warn!("About: library totals: {e}"),
            }
            for p in places(&paths) {
                if !go_on() {
                    return;
                }
                m.sizes.push(measure(&p.path, p.folder, &go_on));
            }
            if go_on() {
                state::post(move |st| shown(st, generation, m));
            }
        })
        .map_err(|e| log::warn!("About: cannot start measuring: {e}"))
        .ok();
}

fn shown(st: &Rc<State>, generation: u64, m: Measured) {
    if GENERATION.load(Ordering::Relaxed) != generation {
        return;
    }
    let Some(app) = st.app.upgrade() else { return };
    let v = app.global::<AboutView>();
    v.set_albums(library::audio::thousands(m.albums.max(0) as u64).into());
    v.set_tracks(library::audio::thousands(m.tracks.max(0) as u64).into());
    v.set_length(total_length(m.seconds).into());
    v.set_size(crate::metadata::view::short_size(m.bytes.max(0) as u64).into());
    v.set_sources(crate::server::sources_line(&m.sources).into());
    v.set_measuring(false);
    show_files(st, Some(&m));
}

/// "Open folder" on a place.
pub fn open(st: &Rc<State>, i: usize) {
    let Some(p) = places(&st.paths).into_iter().nth(i) else { return };
    let ok = if p.folder { crate::explorer::open_folder(&p.path) } else { crate::explorer::show(&p.path.to_string_lossy()) };
    if !ok {
        crate::views::flash(st, &format!("{} is not there yet", p.what));
    }
}

/// "Copy": the report line on the clipboard.
pub fn copy(st: &Rc<State>) {
    let line = this_report_line();
    if crate::explorer::copy_text(&line) {
        crate::views::flash(st, "Copied the version and system details");
    } else {
        crate::views::flash(st, "Could not use the clipboard");
    }
}

// ---- diagnostics ------------------------------------------------------------------------

/// Lines of each log in the diagnostics.
const LOG_LINES: usize = 80;

/// The user's name and home folder taken out of `text` (`~` and `<user>`), so a pasted
/// report names neither.
pub fn scrub(text: &str, home: Option<&str>, user: Option<&str>) -> String {
    let mut out = text.to_owned();
    if let Some(h) = home.filter(|h| h.len() > 3) {
        for form in [h.to_owned(), h.replace('\\', "/"), h.replace('/', "\\")] {
            out = replace_ignore_case(&out, &form, "~");
        }
    }
    if let Some(u) = user.filter(|u| u.chars().count() >= 3) {
        out = replace_ignore_case(&out, u, "<user>");
    }
    out
}

fn replace_ignore_case(text: &str, what: &str, with: &str) -> String {
    if what.is_empty() {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let w = what.to_lowercase();
    // Lower-casing can change lengths outside ASCII; only then fall back to an exact match.
    if lower.len() != text.len() || w.len() != what.len() {
        return text.replace(what, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(at) = lower[i..].find(&w) {
        out.push_str(&text[i..i + at]);
        out.push_str(with);
        i += at + w.len();
    }
    out.push_str(&text[i..]);
    out
}

/// The last `n` lines of a log file, or what kept it from being read.
fn log_tail(path: &Path, n: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            // A crash's report is what matters most: from a little before it, with its backtrace.
            let from = match lines.iter().position(|l| l.contains("FATAL: code panicked")) {
                Some(p) => p.saturating_sub(20),
                None => lines.len().saturating_sub(n),
            };
            lines[from..].iter().take(400).copied().collect::<Vec<_>>().join("\n")
        }
        Err(e) => format!("(not readable: {e})"),
    }
}

/// "Copy diagnostics": one text with everything a report needs, to paste in a message: the
/// version, the system, the renderer, the sound output, the library's size and format, the
/// settings that change what plays, and the end of this run's and the previous run's logs,
/// with the user's name and home folder taken out.
pub fn diagnostics(st: &Rc<State>) -> String {
    let s = st.settings.borrow();
    let mut t = String::new();
    t.push_str(&format!("{}\n", this_report_line()));
    let out = match crate::output::info() {
        Some(o) => format!(
            "{}{}, {} Hz{}{}{}",
            o.device,
            if o.missing { " (the chosen device is not connected)" } else { "" },
            o.rate,
            o.mix_rate.map(|m| format!(", mixed at {m} Hz")).unwrap_or_default(),
            o.exclusive.as_deref().map(|e| format!(", exclusive {e}")).unwrap_or_default(),
            o.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
        ),
        None => "nothing opened yet".into(),
    };
    // Core Audio and PipeWire say how their ring is faring: underruns since the start, and
    // the device's own delay.
    let health = st.player.output_health().map(|h| {
        let ms = |frames: u64| frames as f64 * 1000.0 / f64::from(h.rate.max(1));
        format!("; {} underruns ({} frames lost), device delay {:.1} ms, ring {:.0} ms", h.underruns, h.lost_frames, ms(h.delay), ms(h.target))
    });
    t.push_str(&format!(
        "Sound: {out}{}; device setting {}\n",
        health.unwrap_or_default(),
        s.output_device_name.as_deref().unwrap_or("system default")
    ));
    let (albums, tracks) = library::query::counts(&st.db).unwrap_or((-1, -1));
    let schema: i64 = st.db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(-1);
    let db_size = std::fs::metadata(st.paths.database()).map(|m| m.len()).unwrap_or(0);
    t.push_str(&format!(
        "Library: {albums} albums, {tracks} tracks in {} folder(s); database format {schema} ({}), this build {}\n",
        s.folders.len(),
        crate::metadata::view::short_size(db_size),
        library::db::SCHEMA_VERSION
    ));
    let on = |b: bool| if b { "on" } else { "off" };
    t.push_str(&format!(
        "Settings: exclusive {}, equalizer {}, crossfade {}, ReplayGain {:?}, tag editing {}, lyrics online {}, Last.fm {}, ListenBrainz {}, Discord {}\n",
        on(s.exclusive),
        on(s.equalizer.all.enabled),
        on(s.crossfade.enabled),
        s.gain,
        on(s.tag_editing),
        on(s.lyrics_online),
        on(st.paths.data.join("lastfm.json").exists()),
        crate::connections::listens::diagnostics(st),
        on(s.discord.enabled)
    ));
    drop(s);
    // The server: what it is and how the library stands with it, never its address or user.
    t.push_str(&crate::server::diagnostics_line(st));
    t.push_str(&format!("Data: {}\n", st.paths.config.display()));
    let log = st.paths.log();
    t.push_str(&format!("\n--- this run's log (last lines) ---\n{}\n", log_tail(&log, LOG_LINES)));
    let prev = crate::logging::previous(&log);
    if prev.exists() {
        t.push_str(&format!("\n--- the previous run's log (last lines) ---\n{}\n", log_tail(&prev, LOG_LINES)));
    }
    let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok();
    let user = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).ok();
    scrub(&t, home.as_deref(), user.as_deref())
}

/// "Copy diagnostics": `diagnostics` on the clipboard.
pub fn copy_diagnostics(st: &Rc<State>) {
    let text = diagnostics(st);
    if crate::explorer::copy_text(&text) {
        crate::views::flash(st, "Copied the diagnostics: paste them into your message");
    } else {
        crate::views::flash(st, "Could not use the clipboard");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_name_neither_the_user_nor_their_home() {
        let text = "C:\\Users\\Fakename\\AppData\\Roaming\\MarqueePlayer\\library.db and c:/users/fakename/Music; fakename played";
        let s = scrub(text, Some("C:\\Users\\Fakename"), Some("Fakename"));
        assert_eq!(s, "~\\AppData\\Roaming\\MarqueePlayer\\library.db and ~/Music; <user> played");
        assert_eq!(scrub("/home/sam/.config and sam", Some("/home/sam"), Some("sam")), "~/.config and <user>");
        // A name too short to be told from ordinary words is left to the home folder's removal.
        assert_eq!(scrub("/home/al/x al", Some("/home/al"), Some("al")), "~/x al");
    }

    const ART: usize = 3;
    const LYRICS: usize = 4;

    #[test]
    fn the_version_is_a_date_and_a_count_or_dev() {
        let (date, tail) = VERSION.rsplit_once(['.', '-']).expect("YYYY.M.D.count or YYYY.M.D-dev");
        let parts: Vec<u32> = date.split('.').map(|p| p.parse().expect("numbers")).collect();
        assert_eq!(parts.len(), 3, "{VERSION}");
        assert!(parts[0] >= 2026 && (1..=12).contains(&parts[1]) && (1..=31).contains(&parts[2]), "{VERSION}");
        assert!(tail == "dev" || tail.parse::<u16>().is_ok_and(|n| n >= 1), "{VERSION}");
        assert_eq!(is_dev(), tail == "dev");
        assert!(agent().starts_with("MarqueePlayer/2"));
        assert!(lyrics_agent().ends_with(" (desktop music player)"));
    }

    #[test]
    fn the_report_line_names_everything_a_bug_report_needs() {
        assert_eq!(
            report_line("2026.11.15.3", "portable", "Windows 11 24H2 (build 26100.2605)", "x64", "Skia (Direct3D 12 through wgpu)"),
            "Marquee Player 2026.11.15.3 (portable) \u{b7} Windows 11 24H2 (build 26100.2605) \u{b7} x64 \u{b7} Skia (Direct3D 12 through wgpu)"
        );
        assert!(this_report_line().starts_with(&format!("Marquee Player {VERSION} ({}) \u{b7} {}", crate::edition::get().short(), os_version())));
    }

    #[test]
    fn lengths_read_naturally() {
        assert_eq!(total_length(0.0), "0 min");
        assert_eq!(total_length(42.0 * 60.0 + 10.0), "42 min");
        assert_eq!(total_length(12.0 * 3600.0 + 5.0 * 60.0), "12 h 5 min");
        assert_eq!(total_length(30.0 * 3600.0), "1 day 6 h");
        assert_eq!(total_length(1500.0 * 86400.0 + 4.0 * 3600.0), "1,500 days 4 h");
    }

    #[test]
    fn places_and_their_details() {
        let p = Paths { config: PathBuf::from(r"C:\cfg"), data: PathBuf::from(r"C:\cfg"), state: PathBuf::from(r"C:\cfg"), cache: PathBuf::from(r"C:\cache") };
        let all = places(&p);
        let names: Vec<&str> = all.iter().map(|p| p.what).collect();
        assert_eq!(names, ["Settings", "Theme", "Library database", "Art cache", "Lyrics cache", "Log"]);
        assert_eq!(all[DATABASE].path, p.data.join("library.db"));
        assert_eq!(all[ART].path, p.cache.join("art"));
        assert_eq!(all[LYRICS].path, p.cache.join("lyrics"));
        assert_eq!(all[5].path, p.state.join("marquee-player.log"));
        assert!(place_detail(ART, &all[ART], None).ends_with("Measuring\u{2026}"));
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(breakable_path(&format!("C:{sep}a{sep}b.db")), format!("C:{sep}\u{200b}a{sep}\u{200b}b.db"));
        assert_eq!(place_detail(ART, &all[ART], Some(Some((3 * 1024 * 1024, 1200)))), "Safe to delete: the next scan makes it again \u{b7} 3.0 MB in 1,200 files");
        assert!(place_detail(LYRICS, &all[LYRICS], Some(None)).ends_with("Empty: nothing kept yet"));
        let db = place_detail(DATABASE, &all[DATABASE], Some(Some((2048, 1))));
        assert!(db.ends_with(&format!("Schema {} \u{b7} 2 KB", library::db::SCHEMA_VERSION)), "{db}");
    }

    #[test]
    fn measuring_counts_files_and_the_database_log() {
        let dir = std::env::temp_dir().join(format!("mp-about-{}", std::process::id()));
        let sub = dir.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("one"), [0u8; 100]).unwrap();
        std::fs::write(sub.join("two"), [0u8; 50]).unwrap();
        assert_eq!(measure(&dir, true, &|| true), Some((150, 2)));
        assert_eq!(measure(&dir, true, &|| false), None, "stopped");
        assert_eq!(measure(&dir.join("gone"), true, &|| true), None);
        let db = dir.join("library.db");
        std::fs::write(&db, [0u8; 10]).unwrap();
        std::fs::write(dir.join("library.db-wal"), [0u8; 5]).unwrap();
        assert_eq!(measure(&db, false, &|| true), Some((15, 1)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// About states the terms, and the source once its address is set.
    #[test]
    fn about_names_the_licence_and_the_source() {
        assert!(legal().starts_with("Copyright \u{a9} 2026 Offbeat. Free software under the GNU General Public License, version 3 or later"));
        assert_eq!(legal().contains("\nSource code: "), !SOURCE.is_empty());
    }

    /// The credits' licences are what the packages say, and each is a dependency.
    #[test]
    fn credits_match_cargo_metadata() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = std::process::Command::new(cargo)
            .args(["metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", crate::platform::system::TARGET_TRIPLE])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        let out = match out {
            Ok(o) if o.status.success() => o.stdout,
            Ok(o) => panic!("cargo metadata failed: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => {
                eprintln!("skipped: cannot run cargo ({e})");
                return;
            }
        };
        let meta: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let packages = meta["packages"].as_array().unwrap();
        for c in credits() {
            let found: Vec<&str> = packages.iter().filter(|p| p["name"] == c.package).filter_map(|p| p["license"].as_str()).collect();
            assert!(!found.is_empty(), "{} is not a dependency", c.package);
            assert!(found.iter().all(|l| *l == c.package_licence), "{}: {found:?}, About says {}", c.package, c.package_licence);
        }
    }
}
