//! Marquee Player: local-first, artwork-forward music player.
//!
//! `main` builds the window and the state, then `wire` attaches every UI callback.
//! Pages are filled by `views`, playback is driven by `playback`, scans by `scanning`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod activity;
mod autoscroll;
// The scripted bench and measurement harness (`MP_BENCH`), only with the `bench` feature.
#[cfg(feature = "bench")]
mod bench;
mod connections;
mod covers;
mod crash;
mod crossfade;
mod dnd;
mod edition;
mod eq;
mod explorer;
mod keys;
mod logging;
mod lyrics;
mod media;
mod menus;
mod metadata;
mod nav;
mod opener;
mod output;
mod paths;
mod placement;
mod platform;
mod playback;
mod playlists;
mod queue;
mod rows;
mod scanning;
mod search;
mod selected;
mod selection;
mod server;
mod session;
mod settings;
mod state;
mod theme;
mod ticker;
mod tray;
mod views;
mod visualizer;
mod wakes;
mod zoom;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use library::{ArtCache, ShelfQuery, query};
use player_core::Player as Engine;
use slint::{CloseRequestResponse, ComponentHandle, Timer, TimerMode};

use crate::covers::AlbumModel;
use crate::rows::{QueueModel, TrackModel};
use crate::settings::{CloseAction, GainSetting, ShelfConfig, Settings, TrayClick};
use crate::state::{Lists, Models, State};

// The generated UI (ui/*.slint), compiled in its own crate so that it can be optimized for
// size (see crates/ui); its types are used here as `crate::AppWindow` and so on.
use marquee_ui::*;

fn main() {
    // A panic on this thread unwinds out of the event loop to here, where its message (kept by
    // the hook, `crash`) is shown: not from inside the hook, whose message box would run a
    // modal loop in the middle of a window callback.
    if std::panic::catch_unwind(run).is_err() {
        let message = crash::pending().unwrap_or_else(|| crash::message("Marquee Player ran into an internal error while starting."));
        crash::show(&message);
        crash::die(101);
    }
}

fn run() {
    // `--version`: the name and version, and nothing else (no window, no files). The AppImage's
    // launcher runs it to see that the system's libraries load before starting the app.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--version") {
        println!("{} {}", about::NAME, about::VERSION);
        return;
    }
    let paths = paths::Paths::resolve();
    // The portable edition keeps everything beside itself or nowhere: a folder it cannot
    // write to is said, not swapped for the profile (`edition`).
    // Every other edition too: a data folder it cannot write to (permissions, a full or
    // read-only disk) is said at start, rather than settings and plays quietly not being kept.
    let unwritable = match edition::get().portable() {
        Some(p) if !paths::Paths::overridden() => edition::check_writable(&paths).err().map(|e| edition::not_writable_message(p, &e)),
        _ => edition::check_writable(&paths).err().map(|e| {
            format!(
                "Marquee Player cannot write to the folder where it keeps your library and settings:\n{e}\n\n\
                 Check that the folder is not read-only and that you may change it (and that the disk is not full), then start Marquee Player again."
            )
        }),
    };
    if unwritable.is_none() {
        paths.ensure();
    }
    // To stderr until this is known to be the instance that runs: a second launch must not
    // rewrite the log of the one it hands over to.
    logging::init();
    if let Some(message) = unwritable {
        log::error!("{message}");
        crash::show(&message);
        std::process::exit(1);
    }

    // Audio files, playlist files or folders on the command line (an "Open with", a drop on
    // the exe).
    let open_args = opener::args();
    // One instance per data folder: a second launch brings the first one's window back,
    // and hands it the files it was given.
    match platform::instance::acquire(&paths.config, &open_args) {
        platform::instance::Instance::First(guard) => {
            INSTANCE.with(|i| *i.borrow_mut() = Some(guard));
            logging::open_file(&paths.log());
            crash::install(paths.log());
            log::info!("{} {} ({}) on {}", about::NAME, about::VERSION, edition::get().short(), about::os_version());
            log::info!("config {}, data {}, cache {}, log {}", paths.config.display(), paths.data.display(), paths.cache.display(), paths.state.display());
            wakes::start();
        }
        platform::instance::Instance::Second { signalled } => {
            log::info!(
                "already running on {}; {}",
                paths.config.display(),
                if signalled { "asked it to show its window" } else { "could not reach its window" }
            );
            return;
        }
    }
    platform::devwindow::init();
    // A tag edit a crash or power cut interrupted: finished or undone (its own records only;
    // only the first instance, which no other instance can be saving beside).
    metadata::tagedit::recover_at_start(&paths.edits());

    let (mut settings, settings_problems, settings_replaceable) = Settings::load_with_problems(&paths.settings());
    // First run convenience for testing: MP_MUSIC adds a library folder.
    if let Some(dir) = std::env::var_os("MP_MUSIC") {
        let dir = PathBuf::from(dir);
        if !settings.folders.contains(&dir) {
            settings.folders.push(dir);
        }
    }
    if let Some(v) = std::env::var("MP_VOLUME").ok().and_then(|v| v.parse().ok()) {
        settings.volume = v;
    }
    let (keymap, problems) = keys::map::Keymap::from_overrides(&settings.shortcuts);
    for p in problems {
        log::warn!("settings.json shortcuts: {p}; ignored");
    }

    let mut db = match open_database(&paths.database()) {
        Some(c) => c,
        None => std::process::exit(1),
    };
    // Portable: library folders on its drive follow the drive to a new letter.
    if edition::follow_drive(&mut settings, &mut db) && settings_replaceable {
        settings.save(&paths.settings());
    }
    let art = ArtCache::new(paths.art());
    covers::init(art.clone());

    let glass = select_renderer();
    let app = AppWindow::new().unwrap_or_else(|e| window_start_error(&e, &paths));
    app.global::<Theme>().set_glass_supported(glass);
    // Linux: no decorations from the system (Slint asks again for them whenever the window's
    // properties change, unless the window says `no-frame`).
    app.global::<Chrome>().set_frameless(cfg!(target_os = "linux"));
    set_platform_words(&app);
    app.set_app_icon(tray::icon::app_icon(64));
    theme::accent::install(
        &app,
        theme::accent::Config {
            follow_art: true,
            fixed: ([0xff, 0xb3, 0x47], [0x1a, 0x12, 0x06]),
            fallback: [0xb4, 0xa6, 0xe8],
            transition: Duration::from_millis(1200),
        },
    );
    let theme_watch = theme::install(&app, paths.theme());

    // What the desktop's mixer shows beside the stream (Linux: PipeWire's application name
    // and icon).
    player_core::output::set_identity(about::NAME, platform::APP_ID);
    let (engine, events) = Engine::new();
    // Engine events are handled as they come, whatever the tick rate (ticker.rs).
    let events = ticker::forward(events);
    engine.set_volume(if settings.muted { 0.0 } else { settings.volume });
    engine.set_gain_config(settings.gain_config());
    engine.set_output(output::config(&settings));
    // The shared equalizer setting until the device is known (eq::install picks its own).
    engine.set_eq(&eq::params(&settings.equalizer.all));

    let queue = Rc::new(RefCell::default());
    let st = Rc::new(State {
        app: app.as_weak(),
        paths: paths.clone(),
        settings: RefCell::new(settings),
        settings_dirty: Cell::new(true),
        settings_delay: Cell::default(),
        settings_held: !settings_replaceable,
        db,
        art,
        player: engine,
        events,
        queue: queue.clone(),
        tokens: RefCell::default(),
        next_token: Cell::new(0),
        requested_next: Cell::new(None),
        listening: RefCell::default(),
        play_failures: Cell::new(0),
        history: RefCell::default(),
        lists: Lists {
            album: TrackModel::new(false),
            artist: TrackModel::new(true),
            search: TrackModel::new(true),
            playlist: TrackModel::new(true),
            tracks: TrackModel::new(true),
            smart: TrackModel::new(true),
            preview: TrackModel::new(true),
        },
        models: Models {
            home: RefCell::default(),
            grid: AlbumModel::new(Vec::new()),
            shelf: AlbumModel::new(Vec::new()),
            artist_albums: AlbumModel::new(Vec::new()),
            artist_appears: AlbumModel::new(Vec::new()),
            search_albums: AlbumModel::new(Vec::new()),
            artists: AlbumModel::new(Vec::new()),
            groups: AlbumModel::new(Vec::new()),
            queue: QueueModel::new(queue),
        },
        open_album: Cell::new(None),
        open_playlist: Cell::new(None),
        scanning: Cell::new(false),
        rescan_pending: Cell::new(false),
        pending_paths: RefCell::default(),
        pending_forget: RefCell::default(),
        confirm_removal: Cell::new(false),
        resume: Cell::new(None),
        session: RefCell::default(),
        searcher: search::Searcher::new(paths.database()),
        hwnd: Cell::new(0),
        grid_loaded: Cell::new(false),
        grid: RefCell::default(),
        shelf_seeds: RefCell::new(HashMap::new()),
        watch: RefCell::new(None),
        media: RefCell::new(None),
        keymap: RefCell::new(keymap),
    });
    state::install(st.clone());
    check_database_soon(&st.paths.database());
    if !settings_problems.is_empty() {
        // Said once the window is up: what was reset, and that the old file is kept.
        let what = settings_problems.join("; ");
        state::post(move |st| {
            let kept = if settings_replaceable { "The old file is kept as settings.json.bad." } else { "The file is left as it is, and settings changed now are not saved until the app is started again." };
            views::flash_long(st, &format!("Settings: {what}. {kept}"));
        });
    }
    playlists::smart::install(&st);
    // Smart playlists saved with rules about ratings (no longer kept): those rules dropped.
    if let Err(e) = library::query::forget_rating_rules(&st.db) {
        log::warn!("smart playlists: rules about ratings not dropped ({e})");
    }
    connections::scrobble::install(&st);
    connections::listens::install(&st);
    server::install(&st);
    connections::presence::install(&st);
    lyrics::install(&st);
    server::streaming::install_app(&st);
    visualizer::install(&st);
    tray::anim::install();
    about::install(&st);
    eq::install(&st);
    // The installed edition: setup and uninstall can find it and ask it to quit.
    platform::integration::installed_running(edition::get(), || {
        let _ = slint::quit_event_loop();
    });
    // Which device is the default (an equalizer setting may be its own), now and as it
    // changes.
    output::watch_devices();
    output::rescan_devices();

    wire(&app);
    autoscroll::install(&app);
    keys::edit::refresh(&st);
    views::refresh_player_settings(&st);
    output::refresh(&st);
    views::refresh_folders(&st);
    metadata::tagedit::sync_settings(&st);
    views::refresh_all(&st);
    // The queue as it was at exit, paused where it was (nothing plays until asked).
    session::restore(&st);

    // Position, play counting, settings and session saves: as often as needed (ticker.rs).
    ticker::install();

    // Files from the command line play now, after the restored queue is in place (playlist
    // files among them are imported).
    if !open_args.is_empty() {
        opener::open_given(&st, open_args, opener::Mode::Play);
    } else if std::env::args_os().skip(1).any(|a| a == "--resume-playback") && st.queue.borrow().current().is_some() {
        // Start an explicitly requested listening session at its saved position.
        views::go(&st, Page::NowPlaying);
        playback::play_current(&st);
    }

    // Scan on start (incremental: fast when nothing changed), then watch.
    scanning::rescan(&st);
    scanning::restart_watch(&st);
    // The server's catalog a few seconds later (server/mod.rs), when signed in.
    server::start_soon();

    // Tray: closing the window hides it (unless Settings says Quit); music keeps playing;
    // Quit is in the tray menu. A click plays or pauses (`tray/mod.rs`).
    let has_tray = tray::install();
    app.window().on_close_requested(move || {
        let quit = state::with(|st| st.settings.borrow().close_action == CloseAction::Quit).unwrap_or(false);
        // Hiding needs a tray to come back from: where the desktop shows none (GNOME
        // without an extension, a panel without a system tray), closing quits instead.
        if has_tray && !quit && !bench_run() && platform::system::tray_host() {
            CloseRequestResponse::HideWindow
        } else if bench_run() || request_quit() {
            if bench_run() {
                let _ = slint::quit_event_loop();
            }
            CloseRequestResponse::HideWindow
        } else {
            // Unsaved tag edits: the window stays, asking about them.
            CloseRequestResponse::KeepWindowShown
        }
    });

    // Where the system restores a window through the toolkit (Linux), before it is shown.
    let saved = st.settings.borrow().window;
    placement::before_show(&app, saved);
    if let Err(e) = app.show() {
        window_start_error(&e, &paths);
    }
    // "Start with Windows" starts it in the tray: the window is made (so a second launch
    // and the tray find it) but not shown. Only with a tray to come back from.
    if edition::started_minimized() && has_tray && platform::system::tray_host() && !bench_run() {
        let _ = app.hide();
        log::info!("started in the tray ({})", edition::MINIMIZED_FLAG);
    }
    // The native window exists once the event loop runs. The frame goes first (the
    // title bar should not show for long), media keys a little later.
    {
        // Polled: the window appears a few milliseconds into the event loop.
        let weak = app.as_weak();
        let native = Timer::default();
        native.start(TimerMode::Repeated, Duration::from_millis(5), move || {
            if let Some(app) = weak.upgrade()
                && set_native_icon(&app)
            {
                NATIVE_TIMER.with(|t| t.borrow_mut().take());
            }
        });
        NATIVE_TIMER.with(|t| *t.borrow_mut() = Some(native));
        let weak = app.as_weak();
        Timer::single_shot(Duration::from_millis(200), move || {
            if let Some(app) = weak.upgrade() {
                state::with(|st| media::install(st, &app));
            }
        });
    }
    #[cfg(feature = "bench")]
    bench::install(&app);
    #[cfg(not(feature = "bench"))]
    if let Some(mode) = std::env::var_os("MP_BENCH") {
        log::warn!("MP_BENCH={}: this build has no bench harness (build with --features bench); running normally", mode.to_string_lossy());
    }
    if let Err(e) = slint::run_event_loop_until_quit() {
        log::error!("event loop: {e}");
    }

    session::autosave(&st, true);
    // Downloads stop; what they wrote stays for the next start to go on from.
    server::streaming::shutdown();
    st.settings_dirty.set(true);
    st.flush_settings();
    tray::remove();
    drop(theme_watch);
    drop(st);
    state::drop_state();
    #[cfg(feature = "bench")]
    if let Some(code) = bench::exit_code()
        && code != 0
    {
        std::process::exit(code);
    }
}

/// SQLite's check of the whole library database, off the UI thread at a low priority, once a
/// start. A file damaged inside (its header still reads, so it opened) is not used on: the
/// damage is recorded beside it (`library::db::damaged_marker`), the user is told, and the app
/// closes the normal way; the next start then offers a new library (`open_database`), the
/// damaged file kept. Writing on into a damaged file can only lose more.
fn check_database_soon(path: &std::path::Path) {
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new().name("mp-dbcheck".into()).spawn(move || {
        library::background_thread();
        let t0 = std::time::Instant::now();
        let Err(found) = library::db::check(&path) else {
            log::info!("library.db checked in {:.0} ms: ok", t0.elapsed().as_secs_f64() * 1000.0);
            return;
        };
        // A busy file is not a damaged one (another program reading it): nothing to say.
        if found.contains("locked") || found.contains("busy") {
            log::warn!("library.db could not be checked: {found}");
            return;
        }
        log::error!("library.db is damaged: {found}");
        let _ = std::fs::write(library::db::damaged_marker(&path), &found);
        let text = format!(
            "Marquee Player found its library database damaged ({found}):\n{}\n\n\
             It closes now, before anything more is written into it. When you start it again, it offers to start with a new \
             library, keeping the damaged file beside it; your music files are not touched.",
            path.display()
        );
        state::post(move |_| {
            crash::show(&text);
            let _ = slint::quit_event_loop();
        });
    });
}

/// Open the library database, or say plainly why it cannot be and what to do (None: the app
/// does not start). A damaged one can be set aside for a new library, after asking.
fn open_database(path: &std::path::Path) -> Option<rusqlite::Connection> {
    use library::db::OpenProblem;
    let e = match library::db::open(path) {
        Ok(c) => return Some(c),
        Err(e) => e,
    };
    log::error!("cannot open the library database {}: {e}", path.display());
    let file = path.display();
    let backups = library::db::backups(path);
    let backup_note = match backups.first() {
        Some(b) => format!("\n\nA copy from before the last update is kept as {}.", b.display()),
        None => String::new(),
    };
    let text = match library::db::problem(&e) {
        OpenProblem::Newer { found } => format!(
            "This library was last opened by a newer version of Marquee Player (its database is format {found}; this version reads up to {}).\n\n\
             Start the newer version again, or, to go back to this version, see \u{201c}To go back\u{201d} in the README.{backup_note}\n\nThe library: {file}",
            library::db::SCHEMA_VERSION
        ),
        OpenProblem::Busy => format!(
            "Another program is using the library database, so Marquee Player cannot open it:\n{file}\n\n\
             Close that program (another copy of Marquee Player using the same folder, or a backup or sync tool working on it) and start Marquee Player again."
        ),
        OpenProblem::ReadOnly(why) => format!(
            "Marquee Player cannot write to its library database ({why}):\n{file}\n\n\
             Check that the file and its folder are not read-only and that you may change them, then start Marquee Player again."
        ),
        OpenProblem::Full => format!("The disk holding Marquee Player's library is full:\n{file}\n\nMake some room on it and start Marquee Player again."),
        OpenProblem::Damaged(why) => {
            let ask = format!(
                "The library database is damaged and cannot be read ({why}):\n{file}\n\n\
                 Start with a new library? Your music files are not touched; the damaged file is kept beside the new one, renamed, \
                 and your music folders are read again. Plays, favourites and playlists start afresh.{backup_note}\n\n\
                 Choose No to close Marquee Player and leave everything as it is."
            );
            log::error!("{ask}");
            // A bench run answers as MP_BENCH_ANSWER says (no dialog opens).
            let yes = if bench_run() {
                std::env::var("MP_BENCH_ANSWER").as_deref() == Ok("yes")
            } else {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Marquee Player")
                    .set_description(&ask)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes
            };
            if yes {
                match library::db::set_aside(path) {
                    Ok(aside) => {
                        log::warn!("the damaged library database is kept as {}; starting a new one", aside.display());
                        match library::db::open(path) {
                            Ok(c) => return Some(c),
                            Err(e) => crash::show(&format!("A new library database could not be made either ({e}):\n{file}")),
                        }
                    }
                    Err(e) => crash::show(&format!("The damaged library database could not be renamed ({e}):\n{file}\n\nQuit any program using it and try again.")),
                }
            }
            return None;
        }
        OpenProblem::Other(why) => format!("Marquee Player cannot open its library database ({why}):\n{file}"),
    };
    crash::show(&text);
    None
}

/// A scripted bench run (`MP_BENCH`, in a build with the `bench` feature): closing quits,
/// and nothing opens the browser.
pub fn bench_run() -> bool {
    cfg!(feature = "bench") && std::env::var_os("MP_BENCH").is_some()
}

/// A test override from the environment that sends requests elsewhere (`MP_LASTFM_API`,
/// `MP_LRCLIB_API`: a local mock), honoured only in a build with the bench harness or a test
/// build. A release build never lets the environment point Last.fm's calls, the session key
/// with them, or the lyrics lookups at another host.
pub fn test_override(name: &str) -> Option<String> {
    if cfg!(any(test, feature = "bench")) { std::env::var(name).ok().filter(|v| !v.is_empty()) } else { None }
}

/// A native file dialog may open: never in a bench run, where it would wait for a person
/// (and stop the run) on whatever screen it opened on. Says so in the log.
pub fn dialog_allowed(what: &str) -> bool {
    if bench_run() {
        log::warn!("bench run: the {what} dialog was asked for and not opened");
        return false;
    }
    true
}

/// ReplayGain off, track or album (the player bar's button cycles; Settings sets).
fn set_gain(st: &Rc<State>, gain: GainSetting) {
    st.settings.borrow_mut().gain = gain;
    st.save_settings_soon();
    playback::apply_gain(st);
    views::refresh_player_settings(st);
}

/// Settings > Visualizer > Visualizer, its key, or Now playing's visualizer menu.
pub fn toggle_visualizer(st: &Rc<State>) {
    {
        let mut s = st.settings.borrow_mut();
        s.visualizer = !s.visualizer;
    }
    st.save_settings_soon();
    views::refresh_player_settings(st);
    visualizer::wake(st);
}

/// Skia draws `backdrop-blur` (live frosted glass). Software draws the pre-blurred art.
/// The renderers are tried in the system's order (`platform::system::RENDERERS`).
/// Windows: Skia through wgpu (Direct3D 12), not OpenGL: on AMD's OpenGL driver some of
/// Skia's dual-source-blend shaders fail to link ("all must have location qualifiers"),
/// and the draws that need them, the backdrop blur of a moving panel among them, are
/// silently dropped. Linux: Skia through wgpu (Vulkan), else Skia on OpenGL (EGL or GLX:
/// GPUs and drivers without Vulkan), else software. macOS tries Skia on Metal first,
/// then retained software tiles on Core Animation.
/// Returns whether glass is available.
/// `SLINT_BACKEND` still wins when set (`winit-skia-opengl` for the frame-timing bench,
/// whose rendering notifier the wgpu surface does not call; on Windows that renderer is
/// only in builds with the `bench-opengl` feature).
fn select_renderer() -> bool {
    if let Ok(forced) = std::env::var("SLINT_BACKEND") {
        // Keep the native window chrome and the bench placement with an explicit backend too.
        let lower = forced.to_lowercase();
        if lower == "winit-skia-opengl" && !cfg!(feature = "bench-opengl") && !platform::system::HAS_SKIA_OPENGL {
            log::warn!("SLINT_BACKEND={forced}: this build has no Skia OpenGL renderer");
        } else {
            match platform::explicit_renderer(selector(), &lower).select() {
                Ok(()) => {
                    platform::configure_renderer(lower.strip_prefix("winit-").unwrap_or(&lower));
                    about::set_renderer(&format!("requested {forced}"));
                    return lower.contains("skia") && !lower.contains("software");
                }
                Err(e) => log::warn!("SLINT_BACKEND={forced}: {e}; trying the platform defaults"),
            }
        }
    }
    let mut last = String::new();
    for (name, label) in platform::system::RENDERERS {
        #[cfg(target_os = "macos")]
        if *name == "skia" && !platform::system::has_metal_device() {
            continue;
        }
        #[cfg(target_os = "macos")]
        let renderer = platform::explicit_renderer(selector(), &format!("winit-{name}"));
        #[cfg(not(target_os = "macos"))]
        let renderer = selector().renderer_name((*name).into());
        match renderer.select() {
            Ok(()) => {
                platform::configure_renderer(name);
                about::set_renderer(label);
                return name.contains("skia");
            }
            Err(e) => {
                last = e.to_string();
                log::info!("renderer {name}: {e}");
            }
        }
    }
    log::warn!("preferred renderers unavailable ({last}); trying software");
    let _ = selector().renderer_name("software".into()).select();
    platform::configure_renderer("software");
    about::set_renderer("software renderer");
    false
}

// Some renderers create their device only when the window first opens.
fn window_start_error(error: &slint::PlatformError, paths: &paths::Paths) -> ! {
    let advice = std::env::var("SLINT_BACKEND").map_or_else(
        |_| "Updating the graphics driver or restarting the computer may help.".to_owned(),
        |forced| format!("Remove SLINT_BACKEND={forced} to use the default renderer."),
    );
    crash::fatal_at_start(&format!("Marquee Player could not open its window ({error}).\n\n{advice}\n\nLog: {}", paths.log().display()));
}

/// The winit backend, with the `MP_WINDOW=secondary` placement when asked for.
fn selector() -> slint::BackendSelector {
    let s = slint::BackendSelector::new().backend_name("winit".into());
    if platform::devwindow::enabled() {
        s.with_winit_window_attributes_hook(platform::devwindow::attributes)
    } else {
        s.with_winit_window_attributes_hook(platform::window::attributes)
    }
}

/// The UI's words that name the system ("Show in Explorer", "Exclusive mode"), from the
/// platform (`platform::Words`).
fn set_platform_words(app: &AppWindow) {
    let w = platform::system::words();
    let p = app.global::<PlatformWords>();
    p.set_show_in(w.show_in.into());
    p.set_show_file(w.show_file.into());
    p.set_show_folder(w.show_folder.into());
    p.set_open_folder_in(w.open_folder_in.into());
    p.set_copy_names(w.copy_names.into());
    p.set_copy_label(w.copy_label.into());
    p.set_exclusive_title(w.exclusive_title.into());
    p.set_exclusive_hint(w.exclusive_hint.into());
    p.set_exclusive_more(w.exclusive_more.into());
    p.set_autostart(w.autostart.into());
    p.set_autostart_more(w.autostart_more.into());
}

/// Ctrl+B / Ctrl+Q: pin or unpin a sidebar, the same as its pin button (same state,
/// saved the same way). Pinning a sidebar open by hover keeps it where it is; unpinning
/// one under the pointer leaves it open until the pointer leaves (hover holds it).
fn on_pin_shortcut(side: platform::PinSide) {
    state::with(|st| {
        let Some(app) = st.app.upgrade() else { return };
        let nav = app.global::<Nav>();
        match side {
            platform::PinSide::Left => nav.set_sidebar_pinned(!nav.get_sidebar_pinned()),
            platform::PinSide::Right => nav.set_queue_pinned(!nav.get_queue_pinned()),
        }
        pins_changed(&app);
    });
}

/// A side button or browser key arrived (see `platform::window`): log it, show it in the
/// readout (Ctrl+Shift+D), and navigate.
fn on_nav_input(what: String, nav: Option<platform::NavInput>) {
    state::with(|st| {
        let action = match nav {
            Some(platform::NavInput::Back) => {
                if views::back(st) { "went back" } else { "Back: nothing to go back to" }
            }
            Some(platform::NavInput::Forward) => {
                if views::forward(st) { "went forward" } else { "Forward: nothing to go forward to" }
            }
            None => "",
        };
        let line = if action.is_empty() { what.clone() } else { format!("{what} → {action}") };
        log::info!("input: {line}");
        if let Some(app) = st.app.upgrade() {
            app.global::<Nav>().set_input_readout(line.into());
        }
    });
}

/// The frame (`platform::window`) reporting the maximize button's hover and press, the
/// pointer near the caption buttons, and the maximized state, for the app-drawn caption.
fn on_frame_event(e: platform::FrameEvent) {
    state::with(|st| {
        let Some(app) = st.app.upgrade() else { return };
        let c = app.global::<Chrome>();
        match e {
            platform::FrameEvent::MaxHover(v) => c.set_max_hover(v),
            platform::FrameEvent::MaxPressed(v) => c.set_max_pressed(v),
            platform::FrameEvent::Maximized(v) => c.set_maximized(v),
            platform::FrameEvent::CaptionZone(v) => c.set_caption_zone(v),
        }
    });
}

/// The tray's tooltip and play/pause label follow the player bar (`tray::sync`).
pub fn sync_tray() {
    tray::sync();
}

/// The tray's "About Marquee Player": the window, on Settings > About.
pub fn show_about(app: &AppWindow) {
    show_window(app);
    state::with(|st| views::settings_section(st, settings::nav::ABOUT));
    ticker::kick();
}

/// Quit, asked for by the user (the tray's Quit, the close button set to quit): unless that
/// would lose unsaved tag edits or cut a save short. Then the window comes back with the panel
/// asking about them, and nothing quits. True when it quits.
fn request_quit() -> bool {
    let held = state::with(|st| {
        let why = metadata::view::blocks_quit()?;
        if let Some(app) = st.app.upgrade() {
            show_window(&app);
        }
        metadata::view::close_request(st);
        views::flash_long(st, why);
        Some(())
    })
    .flatten()
    .is_some();
    if !held {
        let _ = slint::quit_event_loop();
    }
    !held
}

/// Show the window (the tray's Show, About): where it was saved, the first time after a
/// start in the tray.
fn show_window(app: &AppWindow) {
    if let Some(hwnd) = state::with(|st| st.hwnd.get()) {
        platform::placement::show_pending(hwnd);
    }
    let was_hidden = !app.window().is_visible();
    let _ = app.show();
    // Back from hours in the tray: the server may have changed meanwhile.
    state::with(|st| server::window_shown(st, was_hidden));
}

/// The tray icon's tooltip as it stands (for the bench).
#[cfg(feature = "bench")]
pub fn tray_tooltip() -> Option<String> {
    tray::tooltip()
}

/// Is there a tray icon (the shell may refuse one)?
pub fn has_tray() -> bool {
    tray::exists()
}

/// Replace the tray icon's picture (`trayanim`).
pub fn set_tray_image(img: slint::Image) {
    tray::set_image(img);
}

/// A sidebar was pinned or unpinned (its button, or Ctrl+B / Ctrl+Q): keep the settings
/// in step.
fn pins_changed(app: &AppWindow) {
    let nav = app.global::<Nav>();
    let (pinned, queue_pinned) = (nav.get_sidebar_pinned(), nav.get_queue_pinned());
    state::with(|st| {
        let mut s = st.settings.borrow_mut();
        if s.sidebar_pinned != pinned || s.queue_pinned != queue_pinned {
            s.sidebar_pinned = pinned;
            s.queue_pinned = queue_pinned;
            drop(s);
            st.save_settings_soon();
        }
    });
}

thread_local! {
    static NATIVE_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    /// The single-instance guard, held for the whole run (see `instance`).
    static INSTANCE: RefCell<Option<platform::instance::Guard>> = const { RefCell::new(None) };
}

/// Windows is ending the session, or an installer's Restart Manager closes the app: save
/// what a normal exit saves, now (the process may end as soon as this returns), and quit
/// when only the app was asked to close.
fn on_end_session(close_app: bool) {
    log::info!("{}: saving", if close_app { "asked to close (Restart Manager)" } else { "the session is ending" });
    state::with(|st| {
        session::autosave(st, true);
        st.settings_dirty.set(true);
        st.flush_settings();
    });
    if close_app {
        let _ = slint::quit_event_loop();
    }
}

/// A second launch asked for the window (`platform::instance`).
fn on_show_request(hwnd: isize) {
    state::with(|st| {
        if let Some(app) = st.app.upgrade() {
            platform::window::bring_back(&app, hwnd);
        }
    });
    ticker::kick();
}

/// The native window exists: the system's hooks (`platform::window::install`: on Windows
/// the taskbar icon from the exe's resource, the window procedure and the app-drawn frame;
/// on Linux the caption's resize edges and buttons, the side buttons, drops), the
/// single-instance mark, the saved placement. False while the window does not exist yet.
fn set_native_icon(app: &AppWindow) -> bool {
    let hooks = platform::Hooks {
        pin: on_pin_shortcut,
        nav: on_nav_input,
        frame: on_frame_event,
        show: on_show_request,
        files: opener::from_outside,
        end_session: on_end_session,
    };
    let Some(hwnd) = platform::window::install(app, hooks) else { return false };
    INSTANCE.with(|i| {
        if let Some(g) = i.borrow().as_ref() {
            g.mark_window(hwnd as *mut std::ffi::c_void);
        }
    });
    state::with(|st| {
        st.hwnd.set(hwnd);
        let saved = st.settings.borrow().window;
        placement::restore(hwnd, saved);
    });
    platform::devwindow::maximize(hwnd as *mut std::ffi::c_void);
    // The saved interface size (if the window's first events have not applied it already).
    zoom::install(app);
    log::info!("window hooks installed; renderer: {}", about::renderer());
    true
}

/// Attach every UI callback.
fn wire(app: &AppWindow) {
    let weak = app.as_weak();
    app.on_full_cover_changed(move |stem| {
        let Some(app) = weak.upgrade() else { return };
        app.global::<Player>().set_full(Default::default());
        if !stem.is_empty() {
            let weak = app.as_weak();
            let wanted = stem.clone();
            covers::with_image(&stem, covers::Kind::Full, move |img| {
                if let Some(app) = weak.upgrade() && app.get_full_cover_stem() == wanted {
                    app.global::<Player>().set_full(img);
                }
            });
        }
    });
    let chrome = app.global::<Chrome>();
    chrome.on_minimize(platform::window::minimize);
    chrome.on_toggle_maximize(platform::window::toggle_maximize);
    chrome.on_close(platform::window::close);
    chrome.on_system_menu(platform::window::system_menu);
    let nav = app.global::<Nav>();
    nav.on_go(|page| {
        state::with(|st| views::go(st, page));
    });
    nav.on_back(|| {
        state::with(views::back);
    });
    nav.on_forward(|| {
        state::with(views::forward);
    });
    let weak = app.as_weak();
    nav.on_pins_changed(move || {
        if let Some(app) = weak.upgrade() {
            pins_changed(&app);
        }
    });

    playlists::smart::wire(app);
    let a = app.global::<Actions>();

    a.on_open_album(|id| {
        state::with(|st| views::open_album(st, i64::from(id)));
    });
    a.on_open_artist(|name| {
        state::with(|st| views::open_artist(st, &name));
    });
    a.on_artist_links(|kind, id, shown| {
        let links = state::with(|st| views::artist_links(st, &kind, id, &shown)).unwrap_or_default();
        slint::ModelRc::new(slint::VecModel::from(links))
    });
    a.on_hero_select(|i, j| views::hero_select(i.max(0) as usize, j.max(0) as usize));
    a.on_open_artist_at(|i| {
        state::with(|st| views::open_artist_at(st, i.max(0) as usize));
    });
    a.on_open_playlist(|id| {
        state::with(|st| views::open_playlist(st, i64::from(id)));
    });

    // Albums.
    a.on_play_album(|id| {
        state::with(|st| {
            let tracks = query::album_tracks(&st.db, i64::from(id)).unwrap_or_default();
            playback::play_tracks(st, tracks, 0);
        });
    });
    a.on_shuffle_album(|id| {
        state::with(|st| {
            let tracks = query::album_tracks(&st.db, i64::from(id)).unwrap_or_default();
            playback::shuffle_tracks(st, tracks);
        });
    });
    a.on_enqueue_album(|id, next| {
        state::with(|st| {
            let tracks = query::album_tracks(&st.db, i64::from(id)).unwrap_or_default();
            playback::enqueue(st, tracks, next);
        });
    });
    a.on_add_album_to_playlist(|album, pid| {
        state::with(|st| {
            let ids: Vec<i64> = query::album_tracks(&st.db, i64::from(album)).unwrap_or_default().iter().map(|t| t.id).collect();
            if let Err(e) = query::add_to_playlist(&st.db, i64::from(pid), &ids) {
                return views::not_written(st, "Could not add to the playlist", &e);
            }
            views::playlists_changed(st);
        });
    });
    a.on_toggle_favourite_album(|id| {
        state::with(|st| {
            let fav = match query::toggle_favourite_album(&st.db, i64::from(id)) {
                Ok(fav) => fav,
                Err(e) => return views::not_written(st, "Could not change the favourite", &e),
            };
            playlists::smart::changed(st);
            if let Some(app) = st.app.upgrade() {
                app.global::<AlbumPage>().set_favourite(fav);
            }
            views::refresh_home(st);
        });
    });

    // Track lists.
    a.on_play_in(|ctx, index| {
        state::with(|st| {
            let Some(list) = st.lists.by_ctx(&ctx) else { return };
            let tracks = list.tracks();
            playback::play_tracks(st, tracks, index.max(0) as usize);
        });
    });
    a.on_shuffle_in(|ctx| {
        state::with(|st| {
            let Some(list) = st.lists.by_ctx(&ctx) else { return };
            playback::shuffle_tracks(st, list.tracks());
        });
    });
    a.on_enqueue_in(|ctx, index, next| {
        state::with(|st| {
            let Some(t) = st.lists.by_ctx(&ctx).and_then(|l| l.get(index as usize)) else { return };
            playback::enqueue(st, vec![t], next);
        });
    });
    a.on_favourite_in(|ctx, index| {
        state::with(|st| {
            let Some(t) = st.lists.by_ctx(&ctx).and_then(|l| l.get(index as usize)) else { return };
            let fav = match query::toggle_favourite_track(&st.db, t.id) {
                Ok(fav) => fav,
                Err(e) => return views::not_written(st, "Could not change the favourite", &e),
            };
            // The lists, the player bar and the server (server/write.rs) in step.
            selected::favourites_written(st, &[t.id], fav);
        });
    });
    // The selection of a track list or the queue ("queue"): selected.rs.
    a.on_select_press(|ctx, row, ctrl, shift| {
        state::with(|st| selected::press(st, &ctx, row.max(0) as usize, ctrl, shift));
    });
    a.on_select_release(|ctx, row| {
        state::with(|st| selected::release(st, &ctx, row.max(0) as usize));
    });
    a.on_select_cursor(|ctx, from, to, extend| {
        state::with(|st| selected::cursor(st, &ctx, from.max(0) as usize, to.max(0) as usize, extend));
    });
    a.on_select_all(|ctx| {
        state::with(|st| selected::all(st, &ctx));
    });
    a.on_select_clear(|ctx| state::with(|st| selected::clear(st, &ctx)).unwrap_or(false));
    a.on_delete_selected(|ctx, at| {
        state::with(|st| selected::delete(st, &ctx, at.max(0) as usize));
    });
    a.on_add_to_playlist(|ctx, index, pid| {
        state::with(|st| {
            let Some(t) = st.lists.by_ctx(&ctx).and_then(|l| l.get(index as usize)) else { return };
            if let Err(e) = query::add_to_playlist(&st.db, i64::from(pid), &[t.id]) {
                return views::not_written(st, "Could not add to the playlist", &e);
            }
            views::playlists_changed(st);
        });
    });

    // Transport.
    a.on_toggle_play(|| {
        state::with(playback::toggle_play);
    });
    a.on_next(|| {
        state::with(playback::next);
    });
    a.on_previous(|| {
        state::with(playback::previous);
    });
    a.on_seek(|secs| {
        state::with(|st| playback::seek(st, f64::from(secs)));
    });
    a.on_scrub(|secs| {
        state::with(|st| playback::scrub(st, f64::from(secs)));
    });
    a.on_seek_by(|d| {
        state::with(|st| playback::seek_by(st, f64::from(d)));
    });
    // Moving the slider while muted unmutes.
    a.on_set_volume(|v| {
        state::with(|st| playback::set_volume(st, v));
    });
    a.on_toggle_mute(|| {
        state::with(playback::toggle_mute);
    });
    a.on_cycle_gain(|| {
        state::with(|st| {
            let next = match st.settings.borrow().gain {
                GainSetting::Off => GainSetting::Track,
                GainSetting::Track => GainSetting::Album,
                GainSetting::Album => GainSetting::Off,
            };
            set_gain(st, next);
        });
    });
    a.on_set_gain_mode(|mode| {
        let gain = match mode {
            0 => GainSetting::Off,
            2 => GainSetting::Album,
            _ => GainSetting::Track,
        };
        state::with(|st| set_gain(st, gain));
    });
    a.on_preamp_step(|delta| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.preamp_db = (s.preamp_db.round() + delta as f32).clamp(-12.0, 12.0);
            }
            st.save_settings_soon();
            playback::apply_gain(st);
            views::refresh_player_settings(st);
        });
    });
    a.on_toggle_prevent_clipping(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.prevent_clipping = !s.prevent_clipping;
            }
            st.save_settings_soon();
            playback::apply_gain(st);
            views::refresh_player_settings(st);
        });
    });
    a.on_toggle_close_action(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.close_action = match s.close_action {
                    CloseAction::Hide => CloseAction::Quit,
                    CloseAction::Quit => CloseAction::Hide,
                };
            }
            st.save_settings_soon();
            views::refresh_player_settings(st);
        });
    });
    a.on_toggle_tray_click(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.tray_click = match s.tray_click {
                    TrayClick::PlayPause => TrayClick::ShowWindow,
                    TrayClick::ShowWindow => TrayClick::PlayPause,
                };
            }
            st.save_settings_soon();
            views::refresh_player_settings(st);
        });
    });
    a.on_output_menu(output::open_menu);
    a.on_lastfm_connect(|key, secret| connections::scrobble::connect(&key, &secret));
    a.on_lastfm_cancel(connections::scrobble::cancel);
    a.on_lastfm_disconnect(|| {
        state::with(connections::scrobble::disconnect);
    });
    a.on_lastfm_retry(connections::scrobble::retry);
    a.on_listenbrainz_connect(|token, server| connections::listens::connect(&token, &server));
    a.on_listenbrainz_disconnect(connections::listens::disconnect);
    a.on_listenbrainz_retry(connections::listens::retry);
    a.on_listenbrainz_token_page(connections::listens::open_token_page);
    a.on_listenbrainz_server_edited(|text| {
        state::with(|st| connections::listens::server_edited(st, &text));
    });
    a.on_toggle_listenbrainz_love(|| {
        state::with(connections::listens::toggle_love);
    });
    a.on_server_connect(|url, user, password| {
        state::with(|st| server::connect(st, &url, &user, &password));
    });
    a.on_server_address_edited(|text| {
        state::with(|st| server::address_edited(st, &text));
    });
    a.on_server_sync(|| {
        state::with(server::sync_now);
    });
    a.on_server_open_sign_in(|| {
        state::with(server::open_sign_in);
    });
    a.on_server_stop(|| {
        state::with(server::stop);
    });
    a.on_server_sign_out(|| {
        state::with(server::sign_out);
    });
    a.on_server_toggle(|which| {
        let which = match which {
            0 => server::Switch::Marks,
            1 => server::Switch::Plays,
            _ => server::Switch::Scrobbles,
        };
        state::with(|st| server::toggle(st, which));
    });
    a.on_server_confirm_removal(|| {
        state::with(server::confirm_removal);
    });
    a.on_discord_set(|enabled, id| {
        state::with(|st| connections::presence::set(st, enabled, &id));
    });
    a.on_lyrics_seek(|i| {
        state::with(|st| lyrics::seek(st, i.max(0) as usize));
    });
    a.on_lyrics_reject(|| {
        state::with(lyrics::reject);
    });
    a.on_lyrics_menu(|x, y, keys| {
        state::with(|st| lyrics::save::menu(st, x, y, keys));
    });
    a.on_lyrics_nudge(|steps| {
        state::with(|st| lyrics::nudge(st, steps, false));
    });
    a.on_lyrics_reset_offset(|| {
        state::with(lyrics::save::reset_offset);
    });
    a.on_lyrics_answer(|answer| {
        state::with(|st| lyrics::save::answer(st, &answer));
    });
    a.on_lyrics_retry(|| {
        state::with(lyrics::retry);
    });
    a.on_toggle_lyrics_online(|| {
        state::with(|st| {
            let on = {
                let mut s = st.settings.borrow_mut();
                s.lyrics_online = !s.lyrics_online;
                s.lyrics_online
            };
            st.save_settings_soon();
            views::refresh_player_settings(st);
            lyrics::set_enabled(st, on);
        });
    });
    a.on_toggle_visualizer(|| {
        state::with(toggle_visualizer);
    });
    a.on_visualizer_wake(|| {
        state::with(visualizer::wake);
    });
    a.on_viz_preset(|i| {
        state::with(|st| visualizer::choose_preset(st, i.max(0) as usize));
    });
    a.on_viz_choose(|what, i| {
        state::with(|st| visualizer::choose(st, &what, i.max(0) as usize));
    });
    a.on_viz_toggle(|what| {
        state::with(|st| visualizer::toggle(st, &what));
    });
    a.on_viz_step(|what, by| {
        state::with(|st| visualizer::step(st, &what, by));
    });
    a.on_viz_custom(|slot, by| {
        state::with(|st| visualizer::custom_step(st, slot.max(0) as usize, by));
    });
    a.on_viz_reset(|| {
        state::with(visualizer::reset);
    });
    a.on_viz_menu(|x, y, keys| {
        state::with(|st| visualizer::menu(st, x, y, keys));
    });
    a.on_viz_presets_menu(|x, y| {
        state::with(|st| visualizer::presets::open_menu(st, x, y));
    });
    a.on_viz_trial(|answer| {
        state::with(|st| visualizer::presets::trial_answer(st, &answer));
    });
    a.on_menu_sub(|i| {
        state::with(|st| menus::open_sub(st, i));
    });
    a.on_toggle_exclusive(|| {
        state::with(output::toggle_exclusive);
    });
    a.on_toggle_limit_peaks(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.limit_peaks = !s.limit_peaks;
            }
            st.save_settings_soon();
            playback::apply_gain(st);
            views::refresh_player_settings(st);
        });
    });
    // Settings > Playback > Crossfade.
    fn crossfade_edit(f: impl FnOnce(&mut settings::CrossfadeSettings)) {
        state::with(|st| {
            f(&mut st.settings.borrow_mut().crossfade);
            st.save_settings_soon();
            views::refresh_player_settings(st);
            playback::crossfade_changed(st);
        });
    }
    a.on_crossfade_toggle(|| crossfade_edit(|c| c.enabled = !c.enabled));
    a.on_crossfade_step(|d| {
        crossfade_edit(|c| c.secs = (c.secs.round() + d.signum() as f32).clamp(crossfade::MIN_SECS, crossfade::MAX_SECS));
    });
    a.on_crossfade_album_toggle(|| crossfade_edit(|c| c.album_gapless = !c.album_gapless));
    a.on_crossfade_skip_toggle(|| crossfade_edit(|c| c.on_skip = !c.on_skip));

    // Settings > Playback > Music from the server (server/streaming.rs).
    a.on_stream_cap_step(|d| {
        state::with(|st| server::streaming::step_cap(st, d));
    });
    a.on_stream_clear(|| {
        server::streaming::clear_unused();
    });

    // Settings > Equalizer (eq.rs).
    a.on_eq_toggle(|| {
        state::with(eq::toggle);
    });
    a.on_eq_gain(|i, db| {
        state::with(|st| eq::set_gain(st, i.max(0) as usize, db));
    });
    a.on_eq_preset_menu(|x, y| {
        state::with(|st| eq::open_menu(st, x, y));
    });
    a.on_eq_mode(|i| {
        state::with(|st| eq::set_mode(st, i == 1));
    });
    a.on_eq_reset(|| {
        state::with(eq::reset);
    });
    a.on_eq_band_kind(|i, k| {
        state::with(|st| eq::set_kind(st, i.max(0) as usize, k));
    });
    a.on_eq_band_step(|i, field, d| {
        state::with(|st| eq::step_band(st, i.max(0) as usize, &field, d));
    });
    a.on_eq_band_remove(|i| {
        state::with(|st| eq::remove_band(st, i.max(0) as usize));
    });
    a.on_eq_band_add(|| {
        state::with(eq::add_band);
    });
    a.on_eq_preamp_mode(|i| {
        state::with(|st| eq::set_preamp_mode(st, i == 1));
    });
    a.on_eq_preamp_step(|d| {
        state::with(|st| eq::step_preamp(st, d));
    });
    a.on_eq_per_device(|| {
        state::with(eq::toggle_per_device);
    });
    a.on_eq_copy(|| {
        state::with(|st| eq::copy(st, eq::CopyAs::Apo));
    });
    a.on_eq_copy_menu(|x, y| {
        state::with(|st| eq::open_copy_menu(st, x, y));
    });
    a.on_eq_paste(|| {
        state::with(eq::paste);
    });
    a.on_eq_band_kind_menu(|i, x, y| {
        state::with(|st| eq::open_kind_menu(st, i.max(0) as usize, x, y));
    });
    a.on_eq_preview_accept(|| {
        state::with(eq::preview_accept);
    });
    a.on_eq_preview_cancel(|| {
        state::with(eq::preview_cancel);
    });
    a.on_open_equalizer(|| {
        state::with(eq::open_panel);
    });
    a.on_cycle_repeat(|| {
        state::with(playback::cycle_repeat);
    });
    a.on_toggle_shuffle(|| {
        state::with(playback::toggle_shuffle);
    });
    a.on_toggle_favourite_current(|| {
        state::with(playback::toggle_favourite_current);
    });

    // Queue.
    a.on_queue_play(|i| {
        state::with(|st| playback::queue_play(st, i.max(0) as usize));
    });
    a.on_queue_remove(|i| {
        state::with(|st| playback::queue_remove(st, i.max(0) as usize));
    });
    a.on_queue_clear(|| {
        state::with(playback::queue_clear);
    });

    // Context menus (menus.rs, ui/menu.slint).
    a.on_menu_request(|kind, ctx, index, id, x, y| {
        state::with(|st| menus::request(st, &kind, &ctx, index, id, x, y));
    });
    a.on_menu_request_keys(|kind, ctx, index, id, x, y| {
        state::with(|st| menus::request_keys(st, &kind, &ctx, index, id, x, y));
    });
    a.on_menu_activate(|i, sub| {
        state::with(|st| menus::activate(st, i, sub));
    });
    a.on_menu_key(|k| {
        state::with(|st| menus::key(st, &k));
    });

    // The name prompt of "New playlist…" (playlists/newplaylist.rs) and "View metadata" (metadata/view.rs).
    // The prompt names a playlist, or an equalizer preset when eq.rs asked.
    a.on_prompt_edited(|text| {
        state::with(|st| {
            if eq::prompt_active() {
                eq::prompt_edited(st, &text)
            } else if visualizer::presets::prompt_active() {
                visualizer::presets::prompt_edited(st, &text)
            } else {
                playlists::newplaylist::edited(st, &text)
            }
        });
    });
    a.on_prompt_confirm(|text| {
        state::with(|st| {
            if eq::prompt_active() {
                eq::prompt_confirm(st, &text)
            } else if visualizer::presets::prompt_active() {
                visualizer::presets::prompt_confirm(st, &text)
            } else {
                playlists::newplaylist::confirm(st, &text)
            }
        });
    });
    a.on_prompt_cancel(|| {
        state::with(|st| {
            if eq::prompt_active() {
                eq::prompt_cancel(st)
            } else if visualizer::presets::prompt_active() {
                visualizer::presets::prompt_cancel(st)
            } else {
                playlists::newplaylist::cancel(st)
            }
        });
    });
    a.on_meta_close(|| {
        // With unsaved tag edits it asks first (metadata::view::close_request).
        state::with(metadata::view::close_request);
    });
    a.on_meta_copy(|i| {
        state::with(|st| metadata::view::copy(st, i));
    });
    a.on_meta_copy_all(|| {
        state::with(metadata::view::copy_everything);
    });
    a.on_meta_explore(|| {
        state::with(metadata::view::explore);
    });
    a.on_meta_activate(|i| {
        state::with(|st| metadata::view::activate(st, i));
    });
    a.on_meta_toggle(|i| {
        state::with(|st| metadata::view::toggle(st, i));
    });
    a.on_meta_key(|k| {
        state::with(|st| metadata::view::key(st, &k));
    });
    a.on_meta_back(|| {
        state::with(metadata::view::back);
    });
    a.on_meta_select(|i| {
        state::with(|st| metadata::view::select(st, i));
    });
    a.on_toggle_details(|| {
        state::with(metadata::details::toggle);
    });
    // Tag editing (metadata/view.rs, metadata/tagedit.rs). Each does nothing with tag editing off.
    a.on_meta_edit(|i| {
        state::with(|st| metadata::view::edit_start(st, i));
    });
    a.on_meta_edit_text(|slot, text| {
        state::with(|st| metadata::view::edit_text(st, slot, &text));
    });
    a.on_meta_edit_commit(|| {
        // Enter in a field: kept, and the list has the keyboard again.
        state::with(|st| {
            if metadata::view::edit_commit(st) {
                metadata::view::focus_list(st);
            }
        });
    });
    a.on_meta_edit_cancel(|| {
        state::with(metadata::view::edit_cancel);
    });
    a.on_meta_edit_next(|slot, dir| {
        state::with(|st| metadata::view::edit_next(st, slot, dir));
    });
    a.on_meta_edit_vertical(|dir| {
        state::with(|st| metadata::view::edit_vertical(st, dir));
    });
    a.on_meta_edit_add(|slot| {
        state::with(|st| metadata::view::edit_add_value(st, slot));
    });
    a.on_meta_edit_remove(|slot| {
        state::with(|st| metadata::view::edit_remove_value(st, slot));
    });
    a.on_meta_edit_move(|slot, dir| {
        state::with(|st| metadata::view::edit_move_value(st, slot, dir));
    });
    a.on_meta_remove_field(|i| {
        state::with(|st| metadata::view::remove_field(st, i));
    });
    a.on_meta_review(|| {
        state::with(metadata::view::review);
    });
    a.on_meta_save(|| {
        state::with(metadata::view::save);
    });
    a.on_meta_cancel_save(|| {
        state::with(metadata::view::cancel_save);
    });
    a.on_meta_back_to_edit(|| {
        state::with(metadata::view::back_to_edit);
    });
    a.on_meta_discard(|| {
        state::with(metadata::view::discard);
    });
    a.on_meta_result_done(|| {
        state::with(metadata::view::result_done);
    });
    a.on_meta_undo_result(|| {
        state::with(metadata::view::undo_result);
    });
    a.on_meta_discard_close(|| {
        state::with(metadata::view::discard_and_close);
    });
    a.on_meta_shortcut(|text, ctrl, shift, alt| state::with(|st| metadata::view::shortcut(st, &text, ctrl, shift, alt)).unwrap_or(false));
    a.on_toggle_tag_editing(|| {
        state::with(metadata::tagedit::toggle);
    });
    a.on_clear_edit_history(|| {
        state::with(metadata::tagedit::clear_history);
    });

    // Drag and drop.
    a.on_album_payload(|id| slint::SharedString::from(dnd::Payload::Album(i64::from(id)).encode()).into());
    a.on_track_payload(|ctx, index, track| {
        let index = dnd::Grabbed { index: index.max(0) as usize, track: Some(i64::from(track)) };
        slint::SharedString::from(dnd::Payload::Track { ctx: ctx.to_string(), index }.encode()).into()
    });
    a.on_queue_payload(|i| slint::SharedString::from(dnd::Payload::QueueRow(i.max(0) as usize).encode()).into());
    a.on_accepts(|data| data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)).is_some());
    a.on_queue_drop(|data, at| {
        let Some(payload) = data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)) else { return };
        let at = at.max(0) as usize;
        state::with(|st| match payload {
            dnd::Payload::Album(id) => {
                let tracks = query::album_tracks(&st.db, id).unwrap_or_default();
                playback::queue_insert(st, at, tracks);
            }
            // A dragged row carries the rows chosen with it (selected.rs).
            dnd::Payload::Track { ctx, index } => {
                let tracks = selected::dragged(st, &ctx, index);
                let n = tracks.len();
                playback::queue_insert(st, at, tracks);
                if n > 1 {
                    views::flash(st, &format!("Added {} to the queue", selected::tracks_word(n)));
                }
            }
            dnd::Payload::QueueRow(from) => selected::reorder_queue(st, from, at),
        });
    });

    // Keyboard (keys/shortcuts.rs): a key nothing on the page took, and list cursors.
    a.on_shortcut(|text, ctrl, shift, alt| {
        let Some(s) = state::with(|st| keys::shortcuts::action(&st.keymap.borrow(), &text, ctrl, shift, alt)).flatten() else { return false };
        state::with(|st| keys::shortcuts::run(st, s));
        true
    });
    a.on_list_key(|text, index, count, layout, cols, page| {
        keys::shortcuts::list_key(&text, index, count, keys::shortcuts::Layout::from_index(layout), cols, page).unwrap_or(-1)
    });
    a.on_reveal(|y, h| {
        state::with(|st| views::reveal(st, y, h));
    });

    // Browse.
    a.on_open_group(|name| {
        state::with(|st| views::open_group(st, &name));
    });
    a.on_grid_filter(|f| {
        state::with(|st| views::filter_grid(st, &f));
    });
    a.on_grid_sort_menu(|x, y| {
        state::with(|st| menus::request_sort(st, x, y));
    });
    a.on_grid_toggle_direction(|| {
        state::with(views::flip_grid);
    });
    a.on_search_all(|kind| {
        state::with(|st| views::open_search_all(st, &kind));
    });
    a.on_search_save_smart(|| {
        state::with(playlists::smart::open_from_search);
    });

    // Search after a short pause in typing, not on every keystroke, and off the UI
    // thread (search.rs): a romaji query takes ~22 ms, and typing never waits on it.
    let search_timer = Rc::new(Timer::default());
    a.on_search(move |q| {
        search_timer.start(TimerMode::SingleShot, Duration::from_millis(120), move || {
            state::with(|st| views::search(st, &q));
        });
    });

    // Library.
    a.on_add_folder(|| {
        state::with(scanning::pick_and_add_folder);
    });
    a.on_remove_folder(|i| {
        state::with(|st| scanning::remove_folder(st, i.max(0) as usize));
    });
    a.on_rescan(|| {
        state::with(scanning::rescan);
    });
    a.on_stop_scan(|| {
        state::with(scanning::stop);
    });
    a.on_confirm_removal(|| {
        state::with(scanning::confirm_removal);
    });
    a.on_shelf_open(|index| {
        state::with(|st| views::open_shelf(st, index.max(0) as usize));
    });
    a.on_shelf_page_reshuffle(|| {
        state::with(views::reshuffle_shelf_page);
    });
    a.on_shelf_reshuffle(|index| {
        state::with(|st| views::reshuffle_shelf(st, index.max(0) as usize));
    });
    // Settings > Keys (keys/edit.rs).
    a.on_key_capture(|row, slot| {
        state::with(|st| keys::edit::capture(st, row, slot));
    });
    a.on_key_captured(|text, ctrl, shift, alt| {
        state::with(|st| keys::edit::captured(st, &text, ctrl, shift, alt));
    });
    a.on_key_capture_cancel(|| {
        state::with(keys::edit::cancel);
    });
    a.on_key_clear(|| {
        state::with(keys::edit::clear);
    });
    a.on_key_reassign(|| {
        state::with(keys::edit::reassign);
    });
    a.on_key_reset(|row| {
        state::with(|st| keys::edit::reset(st, row));
    });
    a.on_key_reset_all(|| {
        state::with(keys::edit::reset_all);
    });
    // Settings' sections and its filter box (views.rs, settings/nav.rs).
    a.on_settings_section(|section| {
        state::with(|st| views::settings_section(st, section.max(0) as usize));
    });
    a.on_settings_filter(|text| {
        state::with(|st| views::settings_filter(st, &text));
    });
    a.on_settings_jump(|section, label| {
        state::with(|st| views::settings_jump(st, section.max(0) as usize, &label));
    });
    a.on_about_diagnostics(|| {
        state::with(about::copy_diagnostics);
    });
    a.on_about_copy(|| {
        state::with(about::copy);
    });
    a.on_about_open(|i| {
        state::with(|st| about::open(st, i.max(0) as usize));
    });
    a.on_toggle_autoscroll(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.autoscroll = !s.autoscroll;
            }
            st.save_settings_soon();
            views::refresh_player_settings(st);
        });
    });
    a.on_toggle_autostart(|| {
        state::with(|st| {
            let edition = edition::get();
            let on = !platform::integration::autostart(edition).unwrap_or(false);
            match platform::integration::set_autostart(edition, on) {
                Ok(()) => log::info!("start with the system: {}", if on { "on" } else { "off" }),
                Err(e) => {
                    log::warn!("start with the system: {e}");
                    views::flash(st, &format!("Could not change it: {e}"));
                }
            }
            views::refresh_player_settings(st);
        });
    });
    a.on_choose_default_apps(|| {
        state::with(|st| {
            if !platform::integration::open_default_apps(edition::get()) {
                views::flash(st, "Could not open Windows' Default apps settings");
            }
        });
    });
    a.on_toggle_tray_animation(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.tray_animation = !s.tray_animation;
            }
            st.save_settings_soon();
            views::refresh_player_settings(st);
            tray::anim::wake(st);
        });
    });
    {
        let weak = app.as_weak();
        app.global::<Autoscroll>().on_stop(move || {
            if let Some(app) = weak.upgrade() {
                autoscroll::away(&app);
            }
        });
    }
    a.on_zoom_step(|d| {
        state::with(|st| zoom::step_by(st, d.signum()));
    });
    a.on_toggle_bar_autohide(|| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                s.bar_autohide = !s.bar_autohide;
            }
            st.save_settings_soon();
            views::refresh_player_settings(st);
        });
    });

    // Playlists.
    a.on_create_playlist(|name| {
        state::with(|st| playlists::nameclash::create(st, &name));
    });
    a.on_delete_playlist(|id| {
        state::with(|st| {
            // Not deleted: it stays open.
            if let Err(e) = query::delete_playlist(&st.db, i64::from(id)) {
                return views::not_written(st, "Could not delete the playlist", &e);
            }
            views::playlists_changed(st);
            views::go(st, Page::Playlists);
        });
    });
    a.on_rename_playlist(|id, name| {
        state::with(|st| playlists::nameclash::rename(st, i64::from(id), &name));
    });
    a.on_clash_choose(|choice, for_rest| {
        state::with(|st| playlists::nameclash::choose(st, playlists::nameclash::Choice::from_index(choice), for_rest));
    });
    a.on_playlist_move(|from, at| {
        state::with(|st| views::move_in_playlist(st, from.max(0) as usize, at.max(0) as usize));
    });
    a.on_playlist_reorder(|data, at| {
        let Some(dnd::Payload::Track { ctx, index }) = data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)) else { return };
        if ctx == "playlist" {
            state::with(|st| {
                if let Some(index) = selected::regrab(st, &ctx, index) {
                    selected::reorder_playlist(st, index, at.max(0) as usize);
                }
            });
        }
    });
    a.on_is_playlist_row(|data| {
        matches!(data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)), Some(dnd::Payload::Track { ctx, .. }) if ctx == "playlist")
    });
    a.on_playlist_drop_new(|data, x, y| {
        let Some(payload) = data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)) else { return };
        state::with(|st| playlists::newplaylist::open_from_drop(st, payload, x, y));
    });
    a.on_playlist_drop(|data, pid| {
        let Some(payload) = data.plain_text().ok().and_then(|t| dnd::Payload::decode(&t)) else { return };
        state::with(|st| views::drop_on_playlist(st, payload, i64::from(pid)));
    });
    a.on_new_smart_playlist(|| {
        state::with(playlists::smart::open_new);
    });
    a.on_import_playlist(|| {
        state::with(playlists::file::import_dialog);
    });
    a.on_import_playlist_dismiss(|| {
        state::with(playlists::file::dismiss);
    });
    a.on_playlist_export_menu(|id, x, y| {
        state::with(|st| menus::request_export(st, i64::from(id), x, y));
    });
    a.on_playlist_remove(|index| {
        state::with(|st| selected::remove_from_playlist(st, &[index.max(0) as usize]));
    });

    // Shelf editor.
    a.on_shelf_move(|i, delta| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                let i = i as usize;
                let j = i as i64 + i64::from(delta);
                if i < s.shelves.len() && j >= 0 && (j as usize) < s.shelves.len() {
                    s.shelves.swap(i, j as usize);
                    drop(s);
                    let j = j as usize;
                    views::shelves_moved(st, |k| Some(if k == i { j } else if k == j { i } else { k }));
                }
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_toggle(|i| {
        state::with(|st| {
            if let Some(s) = st.settings.borrow_mut().shelves.get_mut(i as usize) {
                s.visible = !s.visible;
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_remove(|i| {
        state::with(|st| {
            {
                let mut s = st.settings.borrow_mut();
                let i = i as usize;
                if i < s.shelves.len() {
                    s.shelves.remove(i);
                    drop(s);
                    views::shelves_moved(st, |k| if k == i { None } else if k > i { Some(k - 1) } else { Some(k) });
                }
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_layout(|i, layout| {
        state::with(|st| {
            if let Some(s) = st.settings.borrow_mut().shelves.get_mut(i as usize) {
                s.layout = match layout {
                    ShelfLayout::Row => settings::ShelfLayout::Row,
                    ShelfLayout::Grid => settings::ShelfLayout::Grid,
                    ShelfLayout::Hero => settings::ShelfLayout::Hero,
                };
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_limit(|i, limit| {
        state::with(|st| {
            if let Some(s) = st.settings.borrow_mut().shelves.get_mut(i as usize) {
                s.limit = limit.clamp(6, 120) as usize;
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_rows(|i, rows| {
        state::with(|st| {
            if let Some(s) = st.settings.borrow_mut().shelves.get_mut(i as usize) {
                s.rows = rows.clamp(1, settings::GRID_ROWS_MAX as i32) as usize;
            }
            views::shelves_changed(st);
        });
    });
    a.on_home_grid_columns(|n| {
        state::with(|st| views::home_grid_cols(st, n.max(1) as usize));
    });
    a.on_shelf_title(|i, title| {
        state::with(|st| {
            if let Some(s) = st.settings.borrow_mut().shelves.get_mut(i as usize) {
                let t = title.trim();
                s.title = (!t.is_empty() && t != s.query.default_title()).then(|| t.to_owned());
            }
            views::shelves_changed(st);
        });
    });
    a.on_shelf_add(|kind, param| {
        state::with(|st| {
            let q = match kind.as_str() {
                "recently_added" => ShelfQuery::RecentlyAdded,
                "recently_released" => ShelfQuery::RecentlyReleased,
                "recently_played" => ShelfQuery::RecentlyPlayed,
                "most_played" => ShelfQuery::MostPlayed,
                "random" => ShelfQuery::Random,
                "favourites" => ShelfQuery::Favourites,
                "genre" => ShelfQuery::Genre { name: param.to_string() },
                "label" => ShelfQuery::Label { name: param.to_string() },
                "playlist" => match param.parse() {
                    Ok(id) => ShelfQuery::Playlist { id },
                    Err(_) => return,
                },
                _ => return,
            };
            st.settings.borrow_mut().shelves.push(ShelfConfig::new(q, settings::ShelfLayout::Row, 18));
            views::shelves_changed(st);
        });
    });
    a.on_shelves_reset(|| {
        state::with(views::reset_shelves);
    });
}
