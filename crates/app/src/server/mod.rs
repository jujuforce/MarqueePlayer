//! Settings > Library > Server: a Plex or Subsonic server as a library source
//! beside the music folders (docs/features/navidrome.md). This is the UI thread's side: signing in and
//! out, when a sync runs and what Settings, the sidebar and the pill say of it. The work is
//! `server/sync.rs`'s, on threads of its own; the database side `library::remote`'s; the sign-in
//! kept between runs `server/account.rs`'s.
//!
//! **The client is read-only** (`Client::new(.., true)`) for the sync, the covers and streaming.
//! Only `server/write.rs` writes, with a client allowed the kinds the switches below allow
//! (favourites, plays), and never anything else: no playlist, no delete, no tag, no
//! scan. With both switches off nothing is written to the server at all.
//!
//! **When a sync runs**: at start (a few seconds after the window is up, never in its way),
//! after signing in, on Sync now, and when the window comes back from the tray after hours away
//! (`AWAY`); never on a timer. Each first asks the server whether it has scanned since the last
//! sync that saw everything, and lists nothing when it has not (except sign-in and Sync now,
//! which list anyway). One runs at a time; one asked for meanwhile runs after it.
//!
//! **What is said**: a server not answering is a quiet line in Settings (the library stays as it
//! was last synced), never a dialog; a sign-in the server refuses asks to sign in again (and
//! marks the section); what the shrink guard kept asks to confirm, and so does another server's
//! music in the library when the server signed in to is not it (`Hold`). The address, the user and the
//! token never reach the log (the log names what happened, not where); the password is never
//! kept at all.
//!
//! **Signing out** asks nothing: it removes the server's music from the library (tracks only the
//! server has, with their plays; the folders' tracks only lose their link to the server), the
//! stored sign-in and (with streaming) the stream cache. Nothing of the folders' music changes.
//!
//! The area's files: `sync` (the work on threads), `write` (the writes the switches allow),
//! `account` (the sign-in kept between runs), `streaming` (tracks played as they download)
//! and `streammock` (a stand-in server for the tests and the bench).

pub(crate) mod account;
pub(crate) mod browser;
pub(crate) mod libraries;
pub(crate) mod plex;
pub(crate) mod provider;
pub(crate) mod streaming;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod streammock;
pub(crate) mod sync;
pub(crate) mod write;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use library::remote;
use slint::ComponentHandle;
use subsonic::Error;

use crate::server::account::{About, Account, Loaded, Session};
use crate::scanning::count;
use crate::server::sync::{Job, Outcome, Step};
use crate::state::{self, State};
use crate::views;

/// How long after start the first sync waits: the window, the restored queue and the start-up
/// scan come first.
const START_DELAY: Duration = Duration::from_secs(4);
/// The window shown again after this long hidden (in the tray) looks at the server again.
pub const AWAY: Duration = Duration::from_secs(3 * 3600);

/// Why a sync runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Start,
    SignIn,
    Manual,
    Shown,
    Confirm,
}

impl Reason {
    /// List even when the server says it has not scanned (the user asked).
    fn force(self) -> bool {
        matches!(self, Reason::SignIn | Reason::Manual | Reason::Confirm)
    }
    /// The outcome goes in the pill too.
    fn told(self) -> bool {
        matches!(self, Reason::SignIn | Reason::Manual | Reason::Confirm)
    }
}

/// What a library holds of the server, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub counts: remote::Counts,
    pub albums: i64,
    pub synced_at: Option<i64>,
    pub connected: bool,
}

pub fn facts(conn: &rusqlite::Connection) -> Facts {
    let server = remote::server(conn).ok().flatten();
    Facts {
        counts: remote::counts(conn).unwrap_or_default(),
        albums: conn.query_row("SELECT COUNT(*) FROM albums WHERE remote_id IS NOT NULL", [], |r| r.get(0)).unwrap_or(0),
        synced_at: server.as_ref().and_then(|s| s.synced_at),
        connected: server.is_some(),
    }
}

/// The server's address as the cloud's tooltip says it (`Server.host`): without its scheme or a
/// trailing slash, `https://music.example.org/nd/` as `music.example.org/nd`.
pub fn host_of(url: &str) -> String {
    let url = url.trim();
    let rest = ["https://", "http://"]
        .iter()
        .find_map(|scheme| url.get(..scheme.len()).filter(|s| s.eq_ignore_ascii_case(scheme)).map(|_| &url[scheme.len()..]))
        .unwrap_or(url);
    rest.trim_end_matches('/').to_owned()
}

/// The server to name on retained tracks while signed out. Plex identities do not store an address.
fn catalog_host(catalog: Option<&remote::Server>) -> String {
    let Some(catalog) = catalog else { return String::new() };
    if provider::is_plex_identity(&catalog.identity) {
        return "Plex Media Server".into();
    }
    host_of(catalog.identity.split_once(" (").map_or(catalog.identity.as_str(), |(url, _)| url))
}

/// What a sync kept in the library until the user confirms its removal (Settings' "Remove it
/// from the library": `confirm_removal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Most of the catalog is no longer listed (the shrink guard).
    Shrink(remote::Held),
    /// The server signed in to is not the one whose music the library shows.
    Other(sync::Elsewhere),
}

#[derive(Default)]
struct Ctl {
    /// Signed in: what signs requests, what the server is, and the client made of them.
    signed_in: Option<Session>,
    /// A new account can sync while another account's retained catalog awaits confirmation.
    catalog_active: bool,
    /// Bumped by each Connect and Sign out: an older sign-in's answer is dropped.
    attempt: u64,
    signing_in: bool,
    plex_login: Option<browser::Login>,
    plex_libraries: Vec<plex::MusicLibrary>,
    /// The running sync's Stop, its reason and generation.
    sync: Option<(Arc<AtomicBool>, Reason, u64)>,
    generation: u64,
    /// A sync asked for while one runs.
    pending: Option<Reason>,
    signing_out: bool,
    /// When the last sync ended, however it ended.
    checked: Option<Instant>,
    /// Said under the status (quietly), and whether the user must act.
    problem: Option<(String, bool)>,
    held: Option<Hold>,
    step: Option<Step>,
    facts: Facts,
    /// Where the catalog in the library came from (`host_of` its address), signed in or not.
    catalog_host: String,
    /// Remote artwork requested or received since this catalog was connected. A snapshot
    /// taken before the download (such as a search result) can still resolve its cover.
    covers: HashMap<String, Option<String>>,
}

thread_local! {
    static CTL: RefCell<Ctl> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut Ctl) -> R) -> R {
    CTL.with(|c| f(&mut c.borrow_mut()))
}

/// The library's identity for this server and user (`remote::sync`): one catalog at a time.
pub(crate) fn identity(c: &subsonic::Credentials) -> String {
    account::subsonic_identity(&c.url, &c.user)
}

/// Playback, covers and writeback may use a connection only once its catalog is the one
/// shown in the library. Sync alone gets the new connection while replacement is pending.
fn update_catalog_clients(st: &State) {
    let stored = remote::server(&st.db);
    let changed = with(|c| {
        let active = c.signed_in.as_ref().is_some_and(|s| match &stored {
            Ok(Some(catalog)) => catalog.identity == s.account.identity(),
            Ok(None) => true,
            Err(_) => false,
        });
        if c.catalog_active == active {
            return None;
        }
        c.catalog_active = active;
        c.covers.clear();
        Some(c.signed_in.as_ref().filter(|_| active).map(|s| s.client.clone()))
    });
    if let Some(client) = changed {
        sync::covers_client(client.clone());
        write::set_client(client.as_ref().and_then(|c| c.as_subsonic()).cloned());
        streaming::set_client(client);
    }
}

fn restore_fields(st: &State, account: &Account) {
    if let Some(app) = st.app.upgrade() {
        let s = app.global::<crate::Server>();
        s.set_plex(account.is_plex());
        s.set_plain_http(subsonic::is_plain_http(&account.url));
        s.set_address(account.url.as_str().into());
        s.set_user(account.user().into());
    }
}

pub fn install(st: &Rc<State>) {
    let art = st.art.clone();
    sync::covers_start(st.paths.database(), art, |generation, updates| {
        state::post(move |st| {
            if sync::covers_current(generation) {
                publish_covers(&updates);
                views::server_covers_written(st, &updates);
            }
        });
    });
    crate::server::write::start(st.paths.database(), |changes| state::post(move |st| crate::server::write::apply(st, &changes)));
    crate::server::write::set_writes(crate::server::write::writes_of(&st.settings.borrow()));
    let loaded = account::load(&account::path(&st.paths.data));
    let f = facts(&st.db);
    let retained = remote::server(&st.db).ok().flatten();
    if let Some(catalog) = &retained && let Some(app) = st.app.upgrade() {
        app.global::<crate::Server>().set_plex(provider::is_plex_identity(&catalog.identity));
    }
    with(|c| {
        c.facts = f;
        c.catalog_host = catalog_host(retained.as_ref());
    });
    match loaded {
        Loaded::SignedIn(session) => {
            with(|c| c.signed_in = Some(*session));
            update_catalog_clients(st);
        }
        Loaded::Locked(account) => {
            with(|c| {
                c.problem = Some((
                    "The saved sign-in was made on another computer or for another Windows account, and cannot be read here. Sign in again to keep the server's music up to date.".into(),
                    true,
                ))
            });
            restore_fields(st, &account);
        }
        Loaded::None => {}
    }
    libraries::restore(st);
    show(st);
}

/// After the window is up: the start's sync, a little later.
pub fn start_soon() {
    if with(|c| c.signed_in.is_none()) {
        return;
    }
    slint::Timer::single_shot(START_DELAY, || {
        state::with(|st| sync(st, Reason::Start));
    });
}

/// The window came back (from the tray). `was_hidden`: it was not on screen before.
pub fn window_shown(st: &Rc<State>, was_hidden: bool) {
    let due = with(|c| c.signed_in.is_some() && c.sync.is_none() && c.checked.is_some_and(|t| t.elapsed() >= AWAY));
    if was_hidden && due {
        sync(st, Reason::Shown);
    }
}

/// The address field changed: say whether it is plain http.
pub fn address_edited(st: &Rc<State>, text: &str) {
    if let Some(app) = st.app.upgrade() {
        app.global::<crate::Server>().set_plain_http(subsonic::is_plain_http(text));
    }
}

/// Connect: sign in with what was typed (on a thread of its own), then sync.
pub fn connect(st: &Rc<State>, url: &str, user: &str, password: &str) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<crate::Server>();
    let plex = s.get_plex();
    if with(|c| c.signed_in.is_some() || c.signing_in || c.signing_out || c.plex_login.is_some()) {
        return;
    }
    let (url, user) = (url.trim().to_owned(), user.trim().to_owned());
    let missing = if url.is_empty() {
        Some("Type the server's address: the one you open it at in a browser.")
    } else if !plex && user.is_empty() {
        Some("Type your user name on the server.")
    } else if plex && password.trim().is_empty() {
        Some("Enter your Plex access token.")
    } else if password.is_empty() {
        Some("Type your password on the server.")
    } else {
        None
    };
    if let Some(m) = missing {
        s.set_error(m.into());
        return;
    }
    if let Err(e) = subsonic::normalize_url(&url) {
        s.set_error(sign_in_words(&e, plex).into());
        return;
    }
    let password = password.to_owned();
    // Clear the field before handing the secret to the sign-in thread.
    s.set_password("".into());
    s.set_error("".into());
    s.set_signing_in(true);
    let attempt = with(|c| {
        c.attempt += 1;
        c.signing_in = true;
        c.attempt
    });
    log::info!("server: signing in");
    let spawned = std::thread::Builder::new().name("mp-server-signin".into()).spawn(move || {
        let result = account::sign_in(plex, &url, &user, &password);
        drop(password);
        state::post(move |st| signed_in(st, attempt, plex, result));
    });
    if let Err(e) = spawned {
        with(|c| c.signing_in = false);
        s.set_signing_in(false);
        s.set_error(format!("Could not start signing in ({e}).").into());
    }
}

fn signed_in(st: &Rc<State>, attempt: u64, plex: bool, result: Result<(Session, String), Error>) {
    if with(|c| c.attempt != attempt) {
        return;
    }
    with(|c| c.signing_in = false);
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<crate::Server>();
    s.set_signing_in(false);
    match result {
        Ok((session, secret)) => {
            start_session(st, session, &secret);
        }
        Err(e) => {
            log::info!("server: sign-in failed ({})", class(&e));
            s.set_error(sign_in_words(&e, plex).into());
        }
    }
}

fn start_session(st: &Rc<State>, session: Session, secret: &str) {
    log::info!("server: signed in ({})", session.about.describe());
    if !account::save(&account::path(&st.paths.data), &session, secret) {
        views::flash_long(st, "Signed in, but the sign-in could not be saved: the next start asks for it again");
    }
    with(|c| {
        c.signed_in = Some(session);
        c.problem = None;
        c.held = None;
    });
    if let Some(app) = st.app.upgrade() {
        let s = app.global::<crate::Server>();
        s.set_password("".into());
        s.set_error("".into());
        s.set_plex_manual(false);
    }
    update_catalog_clients(st);
    libraries::restore(st);
    show(st);
    sync(st, Reason::SignIn);
}

/// Home's "Connect a server": Settings > Library, with the sign-in scrolled into view and its
/// address field focused (the card does that once it is laid out: `SettingsView.focus-server`).
pub fn open_sign_in(st: &Rc<State>) {
    views::settings_section(st, crate::settings::nav::LIBRARY);
    let shown = with(|c| c.signed_in.is_none() && !c.signing_out && c.sync.is_none());
    if shown && let Some(app) = st.app.upgrade() {
        app.global::<crate::SettingsView>().set_focus_server(true);
    }
}

/// A sync runs (its batches reach open pages as a scan's do: `views::library_changed_live`).
pub fn syncing() -> bool {
    with(|c| c.sync.is_some())
}

/// Sync now (Settings).
pub fn sync_now(st: &Rc<State>) {
    sync(st, Reason::Manual);
}

/// Remove what the server no longer lists, which the shrink guard kept.
pub fn confirm_removal(st: &Rc<State>) {
    sync(st, Reason::Confirm);
}

/// Stop the running sync: what it wrote stays.
pub fn stop(st: &Rc<State>) {
    let stopped = with(|c| c.sync.as_ref().map(|(cancel, ..)| !cancel.swap(true, Ordering::Relaxed)).unwrap_or(false));
    if stopped {
        log::info!("server sync: stop asked for");
        if let Some(app) = st.app.upgrade() {
            app.global::<crate::Server>().set_stopping(true);
        }
    }
}

pub fn sync(st: &Rc<State>, reason: Reason) {
    let Some((identity, client)) = with(|c| c.signed_in.as_ref().map(|s| (s.account.identity(), s.client.clone()))) else { return };
    if with(|c| c.signing_out) {
        return;
    }
    if with(|c| c.sync.is_some()) {
        // After this one: the stronger of the two asks.
        with(|c| {
            c.pending = Some(match c.pending {
                Some(p) if p.force() && !reason.force() => p,
                Some(Reason::Confirm) => Reason::Confirm,
                _ => reason,
            })
        });
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let generation = with(|c| {
        c.generation += 1;
        c.sync = Some((cancel.clone(), reason, c.generation));
        c.step = Some(Step::Asking);
        c.generation
    });
    let job = Job {
        marks: client.as_subsonic().is_some() && st.settings.borrow().server_marks,
        client,
        identity,
        db: st.paths.database(),
        force: reason.force(),
        confirm_removal: reason == Reason::Confirm,
        cancel,
    };
    log::info!("server sync: {reason:?}");
    show(st);
    let spawned = std::thread::Builder::new().name("mp-server-sync".into()).spawn(move || {
        library::background_thread();
        let outcome = library::guard::catching(|| {
            sync::run(&job, &|step| {
                state::post(move |st| progressed(st, generation, step));
            })
        })
        .unwrap_or_else(|what| {
            log::error!("server sync panicked: {what}");
            Outcome::Library(format!("internal error ({what})"))
        });
        state::post(move |st| finished(st, generation, outcome));
    });
    if let Err(e) = spawned {
        finished(st, generation, Outcome::Library(format!("cannot start ({e})")));
    }
}

fn progressed(st: &Rc<State>, generation: u64, step: Step) {
    if with(|c| c.sync.as_ref().map(|s| s.2) != Some(generation)) {
        return;
    }
    if step == Step::Written {
        views::library_changed_live(st);
        return;
    }
    if let Step::PlexLibraries(choices) = step {
        libraries::present(st, choices);
        return;
    }
    with(|c| c.step = Some(step));
    show(st);
}

fn finished(st: &Rc<State>, generation: u64, outcome: Outcome) {
    let Some((_, reason, _)) = with(|c| c.sync.take_if(|s| s.2 == generation)) else { return };
    with(|c| {
        c.step = None;
        c.checked = Some(Instant::now());
    });
    update_catalog_clients(st);
    let mut pill: Option<String> = None;
    match outcome {
        Outcome::ChoosePlexLibrary(choices) => libraries::choose_or_prompt(st, choices),
        Outcome::UpToDate(seen) => {
            crate::server::write::after_sync(seen);
            sync::covers_kick();
            log::info!("server sync: up to date (no scan on the server since the last sync)");
            with(|c| c.problem = None);
            if reason.told() {
                pill = Some("The server's music is up to date".into());
            }
        }
        Outcome::Synced { report, failed, complete, seen } => {
            crate::server::write::after_sync(seen);
            log::info!(
                "server sync: {} albums, {} songs ({} changed), {} matched to files, {} removed, {} batches (the longest {:.0} ms), {:.1} s{}{}",
                report.albums,
                report.songs,
                report.changed_songs,
                report.attached,
                report.removed_songs,
                report.batches,
                report.longest_write * 1000.0,
                report.seconds,
                if complete { "" } else { ", not complete" },
                if report.held.is_some() { ", removal held back" } else { "" }
            );
            with(|c| {
                c.held = report.held.map(Hold::Shrink);
                c.problem = (failed > 0).then(|| {
                    (
                        format!("{} could not be read at the last sync; the next one tries again (nothing was removed meanwhile).", if failed == 1 { "1 album".into() } else { format!("{} albums", count(failed)) }),
                        false,
                    )
                });
            });
            if reason.told() {
                pill = Some(match (report.changed_albums, report.removed_albums) {
                    (0, 0) => "Synced with the server: nothing new".into(),
                    (n, 0) => format!("Synced with the server: {} new or changed", albums(n)),
                    (0, r) => format!("Synced with the server: {} removed", albums(r)),
                    (n, r) => format!("Synced with the server: {} new or changed, {} removed", albums(n), albums(r)),
                });
            }
            refresh_library(st);
            sync::covers_kick();
        }
        Outcome::OtherServer(e) => {
            log::info!("server sync: another server's music is in the library; its replacement waits for a confirmation");
            with(|c| {
                c.held = Some(Hold::Other(e));
                c.problem = None;
            });
            if reason.told() {
                pill = Some("The library shows another server's music: Settings asks before replacing it".into());
            }
        }
        Outcome::Stopped => {
            // What changed here still goes (the server's values were not all read).
            crate::server::write::after_sync(crate::server::write::View::new());
            log::info!("server sync: stopped");
            pill = Some("Sync stopped. What it read stays; the next sync goes on from there".into());
            refresh_library(st);
        }
        Outcome::Server(e) if sync::is_sign_in(&e) => {
            log::warn!("server sync: the server refused the sign-in ({})", class(&e));
            lost_sign_in(st);
        }
        Outcome::Server(e) => {
            log::info!("server sync: {} (the library stays as last synced)", class(&e));
            let quiet = sync_words(&e);
            with(|c| c.problem = Some((quiet.clone(), false)));
            if reason.told() {
                pill = Some(quiet);
            }
        }
        Outcome::Library(e) => {
            log::warn!("server sync: the library could not be written ({e})");
            with(|c| c.problem = Some((format!("The library could not be written: {e}"), false)));
            pill = Some(format!("Sync failed: {e}"));
        }
    }
    let (f, host) = (facts(&st.db), catalog_host(remote::server(&st.db).ok().flatten().as_ref()));
    with(|c| {
        c.facts = f;
        c.catalog_host = host;
    });
    show(st);
    if let Some(p) = pill {
        views::flash_long(st, &p);
    }
    if let Some(next) = with(|c| c.pending.take()) {
        sync(st, next);
    }
}

fn albums(n: usize) -> String {
    if n == 1 { "1 album".into() } else { format!("{} albums", count(n)) }
}

/// What a sync wrote reaches every page, the queue and View metadata (as a scan's end does).
fn refresh_library(st: &Rc<State>) {
    crate::playback::refresh_queue_tracks(st);
    views::scan_finished(st);
    crate::metadata::view::library_changed(st);
}

/// The server refused the stored sign-in: signed out of it (its music stays, as last synced),
/// asking to sign in again.
fn lost_sign_in(st: &Rc<State>) {
    let account = with(|c| c.signed_in.take().map(|s| s.account));
    libraries::restore(st);
    update_catalog_clients(st);
    account::remove(&account::path(&st.paths.data));
    with(|c| {
        c.pending = None;
        c.problem = Some(("The server no longer accepts the saved sign-in. Sign in again to keep its music up to date.".into(), true));
    });
    if let Some(account) = account {
        restore_fields(st, &account);
    }
}

/// Sign out: the server's music leaves the library (on a thread), the sign-in is forgotten.
pub fn sign_out(st: &Rc<State>) {
    if with(|c| c.signing_out) {
        return;
    }
    browser::cancel(st);
    with(|c| {
        if let Some((cancel, ..)) = &c.sync {
            cancel.store(true, Ordering::Relaxed);
        }
        c.sync = None;
        c.pending = None;
        c.attempt += 1;
        c.signing_in = false;
        c.signed_in = None;
        c.signing_out = true;
        c.problem = None;
        c.held = None;
        c.step = None;
    });
    update_catalog_clients(st);
    account::remove(&account::path(&st.paths.data));
    // Nothing of the sign-in stays in its fields either: not the address and user name (kept only
    // when the server lost the sign-in, to sign in again with: `lost_sign_in`).
    if let Some(app) = st.app.upgrade() {
        let s = app.global::<crate::Server>();
        s.set_plex(false);
        s.set_address("".into());
        s.set_user("".into());
        s.set_password("".into());
        s.set_plain_http(false);
        s.set_error("".into());
    }
    // The stream cache goes with the sign-in.
    libraries::restore(st);
    crate::server::streaming::clear_cache();
    log::info!("server: signed out; removing its music from the library");
    show(st);
    let db = st.paths.database();
    let spawned = std::thread::Builder::new().name("mp-server-signout".into()).spawn(move || {
        library::background_thread();
        let result = {
            let _w = sync::writer();
            library::db::open(&db).and_then(|mut c| remote::disconnect(&mut c)).map_err(|e| e.to_string())
        };
        state::post(move |st| signed_out(st, result));
    });
    if let Err(e) = spawned {
        signed_out(st, Err(e.to_string()));
    }
}

fn signed_out(st: &Rc<State>, result: Result<usize, String>) {
    with(|c| {
        c.signing_out = false;
        c.facts = facts(&st.db);
        c.catalog_host = catalog_host(remote::server(&st.db).ok().flatten().as_ref());
    });
    match result {
        Ok(n) => {
            log::info!("server: {n} tracks only the server had removed");
            views::flash_long(st, &format!("Signed out. {} only the server had left the library; your folders' music is as it was", tracks(n)));
        }
        Err(e) => {
            log::warn!("server: removing its music failed ({e})");
            with(|c| c.problem = Some((format!("Its music could not be removed from the library ({e}). Try again."), true)));
        }
    }
    show(st);
    views::refresh_all(st);
    crate::playback::refresh_queue_tracks(st);
}

fn tracks(n: usize) -> String {
    if n == 1 { "1 track".into() } else { format!("{} tracks", count(n)) }
}

/// Settings > Library > Server's switches. Each takes effect at once on the writes' thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// Sync favourites with the server.
    Marks,
    /// Report plays to the server.
    Plays,
    /// Scrobble server tracks from this app (Last.fm, ListenBrainz; `settings.server_lastfm`).
    Scrobbles,
}

pub fn toggle(st: &Rc<State>, which: Switch) {
    let on = {
        let mut s = st.settings.borrow_mut();
        let v = match which {
            Switch::Marks => &mut s.server_marks,
            Switch::Plays => &mut s.server_plays,
            Switch::Scrobbles => &mut s.server_lastfm,
        };
        *v = !*v;
        *v
    };
    st.save_settings_soon();
    log::info!("server: {which:?} {}", if on { "on" } else { "off" });
    crate::server::write::set_writes(crate::server::write::writes_of(&st.settings.borrow()));
    match (which, on) {
        // Turned on: in step now (a sync that lists nothing when the server has not scanned
        // reads only its stars), and what changed here meanwhile is sent.
        (Switch::Marks, true) => sync(st, Reason::Start),
        // Off: the plays not yet reported are not sent later either.
        (Switch::Plays, false) => match library::marks::clear_plays(&st.db) {
            Ok(n) if n > 0 => log::info!("server: {n} plays not yet reported forgotten"),
            Ok(_) => {}
            Err(e) => log::warn!("server: queued plays not forgotten ({e})"),
        },
        _ => {}
    }
    show(st);
}

fn publish_covers(updates: &[sync::CoverUpdate]) {
    with(|c| {
        for update in updates {
            c.covers.insert(update.remote_art.clone(), Some(update.stem.clone()));
        }
    });
    crate::covers::server_covers_written(updates);
}

/// Resolve an album snapshot against artwork that arrived later. Prioritize an on-screen
/// remote album once when its cover is still missing.
pub fn album_cover(album: &library::Album) -> Option<String> {
    let Some(key) = album.remote_art.as_ref().filter(|_| album.remote_only) else { return album.art.clone() };
    with(|c| {
        if let Some(stem) = c.covers.get(key) {
            return stem.as_ref().or(album.art.as_ref()).cloned();
        }
        if album.art.is_none() && c.signed_in.is_some() {
            c.covers.insert(key.clone(), None);
            sync::covers_want(album.id);
        }
        album.art.clone()
    })
}

// ---- words --------------------------------------------------------------------------------

/// The class of an error, for the log (never its text: it could carry the address).
pub fn class(e: &Error) -> &'static str {
    match e {
        Error::BadUrl => "not an address",
        Error::Network(_) => "no connection",
        Error::NotSubsonic(_) => "not a Subsonic server",
        Error::WrongCredentials { .. } => "sign-in refused",
        Error::NotPermitted(_) => "not permitted",
        Error::NotFound(_) => "not found",
        Error::Server { .. } => "server error",
        Error::BadResponse(_) => "unreadable answer",
        Error::ReadOnly(_) => "read-only",
    }
}

/// Why a sign-in did not work, and what to do.
pub fn sign_in_words(e: &Error, plex: bool) -> String {
    match e {
        Error::NotPermitted(_) if plex => "This token cannot access a music library. Check which music libraries this account can open in Plex.".into(),
        Error::WrongCredentials { .. } if plex => "Plex refused that access token. Enter a token with access to this server.".into(),
        Error::BadUrl | Error::NotFound(_) | Error::NotSubsonic(_) if plex => "Enter the address of your Plex Media Server, such as http://192.168.1.10:32400.".into(),
        Error::BadUrl => "That is not a server address. Type it as you would in a browser: https://music.example.org, or http://192.168.1.10:4533 on your network.".into(),
        Error::Network(m) => format!("No answer from that address ({m}). Check it, and that the server is running and can be reached from this computer."),
        Error::NotSubsonic(m) => {
            format!("Something answered there, but not a Navidrome or Subsonic server ({m}). If the server is under a path, include it (such as /navidrome).")
        }
        Error::WrongCredentials { code: 41, .. } => "The server does not take token sign-in for this user (an LDAP account, for one), so this app cannot sign in as it.".into(),
        Error::WrongCredentials { code, .. } if *code > 41 => "The server asks for another way of signing in, which this app does not support.".into(),
        Error::WrongCredentials { .. } => "The server did not accept that user name and password.".into(),
        Error::NotPermitted(_) => "The server does not let this user read its library.".into(),
        Error::NotFound(_) | Error::Server { .. } | Error::BadResponse(_) | Error::ReadOnly(_) => format!("The server could not sign you in: {e}."),
    }
}

/// Why a sync did not reach the server (quiet: the library stays as last synced).
pub fn sync_words(e: &Error) -> String {
    let why = match e {
        Error::Network(m) => format!("no answer from the server ({m})"),
        Error::NotSubsonic(m) => format!("something else answers at its address ({m})"),
        Error::NotPermitted(_) => "the server no longer lets this user read its library".into(),
        Error::Server { http_status: Some(s), .. } if (500..600).contains(s) => format!("the server is not working right now (HTTP {s})"),
        other => other.to_string(),
    };
    format!("Could not sync at the last try: {why}. The library shows its music as last synced.")
}

/// "just now", "12 min ago", "3 h ago", "yesterday", "5 days ago".
pub fn ago(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    match mins {
        0 => "just now".into(),
        1..=59 => format!("{mins} min ago"),
        60..=1439 => format!("{} h ago", mins / 60),
        1440..=2879 => "yesterday".into(),
        _ => format!("{} days ago", mins / 1440),
    }
}

/// The status card: (title, text) when signed in and idle.
pub fn describe(user: &str, about: &About, f: &Facts, plain_http: bool, now_ms: i64) -> (String, String) {
    let title = if user.is_empty() { format!("Connected to {}", about.describe()) } else { format!("Signed in as {user} on {}", about.describe()) };
    let on_server = f.counts.server + f.counts.both;
    let mut text = if on_server == 0 && f.synced_at.is_none() {
        "Not synced yet.".to_owned()
    } else {
        let mut t = format!(
            "{} {}, {} {} on the server",
            count(f.albums.max(0) as usize),
            if f.albums == 1 { "album" } else { "albums" },
            count(on_server.max(0) as usize),
            if on_server == 1 { "track" } else { "tracks" }
        );
        if f.counts.both > 0 {
            t.push_str(&format!("; {} of them also in your folders", count(f.counts.both as usize)));
        }
        t.push('.');
        if let Some(at) = f.synced_at {
            t.push_str(&format!(" Synced {}.", ago(now_ms - at)));
        }
        t
    };
    if plain_http {
        text.push_str(" The connection is not encrypted (plain http).");
    }
    (title, text)
}

/// While a sync runs: (title, text, progress, sidebar line).
pub fn describe_step(step: &Step) -> (String, String, f32, String) {
    let title = "Syncing with the server".to_owned();
    match step {
        Step::Replacing => (title, "Removing the other server's music from the library first\u{2026}".into(), -1.0, "Syncing\u{2026}".into()),
        Step::Asking | Step::PlexLibraries(_) => (title, "Asking the server what changed\u{2026}".into(), -1.0, "Syncing\u{2026}".into()),
        Step::Listing { albums } => (title, format!("Listing the server's albums: {} so far", count(*albums)), -1.0, "Syncing\u{2026}".into()),
        Step::Reading { done, total } => {
            let p = if *total > 0 { *done as f32 / *total as f32 } else { -1.0 };
            (title, format!("{} of {} albums read", count(*done), count(*total)), p, format!("Syncing \u{b7} {}%", (p.max(0.0) * 100.0).floor() as u32))
        }
        Step::Waiting => (title, "Waiting for the scan of your folders to finish\u{2026}".into(), -1.0, "Syncing (waiting for the scan)\u{2026}".into()),
        Step::Finishing => (title, "Matching with your folders' music\u{2026}".into(), 1.0, "Syncing \u{b7} 100%".into()),
        Step::Written => (title, String::new(), -1.0, "Syncing\u{2026}".into()),
    }
}

/// What a sync kept, and why.
pub fn hold_text(h: &Hold) -> String {
    match h {
        Hold::Shrink(h) => held_text(h),
        Hold::Other(e) => other_text(e),
    }
}

/// The library shows another server's music than the one signed in to.
pub fn other_text(e: &sync::Elsewhere) -> String {
    let plays = match e.plays {
        0 => String::new(),
        1 => " 1 play not yet reported to it would not be either.".into(),
        n => format!(" {} plays not yet reported to it would not be either.", count(n)),
    };
    format!(
        "This server's music is not the music the library shows from the server signed in to before: its {} {} stay in the library (with their plays, favourites and playlists) until you remove them to make way for this server's.{plays} If it is the same server under another address, sign in with the address used before instead.",
        count(e.songs),
        if e.songs == 1 { "track" } else { "tracks" }
    )
}

/// What the guard kept.
pub fn held_text(h: &remote::Held) -> String {
    format!(
        "{} of the {} tracks the server had are no longer listed by it. They stay in the library (with their plays, favourites and playlists) in case the server is only missing a drive; remove them if they were deleted on purpose.",
        count(h.missing),
        count(h.known)
    )
}

/// Settings, the sidebar and the section list in step with the state.
pub fn show(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<crate::Server>();
    {
        let set = st.settings.borrow();
        s.set_sync_marks(set.server_marks);
        s.set_report_plays(set.server_plays);
        s.set_lastfm_scrobbles(set.server_lastfm);
    }
    CTL.with(|c| {
        let c = c.borrow();
        if let Some(session) = &c.signed_in {
            s.set_plex(session.account.is_plex());
        }
        s.set_signed_in(c.signed_in.is_some());
        s.set_signing_in(c.signing_in);
        s.set_busy(c.sync.is_some() || c.signing_out);
        s.set_stopping(c.sync.as_ref().is_some_and(|s| s.0.load(Ordering::Relaxed)));
        s.set_held(c.held.as_ref().map(hold_text).unwrap_or_default().into());
        // The cloud's tooltip: the server signed in to, or the one whose music is still shown.
        let host = match &c.signed_in {
            Some(session) if c.catalog_active => host_of(&session.account.url),
            _ if c.facts.connected => c.catalog_host.clone(),
            _ => String::new(),
        };
        s.set_host(host.into());
        let (problem, attention) = c.problem.clone().unwrap_or_default();
        s.set_attention(attention);
        if c.signing_out {
            s.set_title("Signing out".into());
            s.set_status("Removing the server's music from the library\u{2026} Your folders' music stays as it is.".into());
            s.set_detail("".into());
            s.set_progress(-1.0);
            s.set_short("Signing out of the server\u{2026}".into());
        } else if let (Some(_), Some(step)) = (&c.signed_in, &c.step) {
            let (title, text, progress, short) = describe_step(step);
            s.set_title(title.into());
            s.set_status(text.into());
            s.set_detail("".into());
            s.set_progress(progress);
            s.set_short(short.into());
        } else if let Some(session) = &c.signed_in {
            let (title, text) = describe(session.account.user(), &session.about, &c.facts, subsonic::is_plain_http(&session.account.url), library::db::now_ms());
            s.set_title(title.into());
            s.set_status(text.into());
            s.set_detail(problem.clone().into());
            s.set_progress(-1.0);
        }
        // Signed out with a server's music still in the library.
        let leftover = if c.signed_in.is_none() && !c.signing_out && c.facts.connected {
            let mut t = if problem.is_empty() { "The library still shows a server's music, as last synced. Sign in again to keep it up to date.".to_owned() } else { problem.clone() };
            t.push_str(" Or remove its music from the library.");
            t
        } else {
            String::new()
        };
        s.set_leftover(leftover.into());
    });
    // The top right of Home: a sync began, went on or ended.
    crate::activity::changed(st);
}

// ---- Settings > About and Copy diagnostics ------------------------------------------------

/// About's line of where the tracks are ("" with no server): local, on the server, both.
pub fn sources_line(c: &remote::Counts) -> String {
    if c.server == 0 && c.both == 0 {
        return String::new();
    }
    format!("{} only in your folders, {} only on the server, {} in both", count(c.local.max(0) as usize), count(c.server.max(0) as usize), count(c.both.max(0) as usize))
}

/// Copy diagnostics' line: what the server is and how the library stands with it; never its
/// address or the user.
pub fn diagnostics_line(st: &Rc<State>) -> String {
    let f = facts(&st.db);
    let (signed_in, about) = with(|c| (c.signed_in.is_some(), c.signed_in.as_ref().map(|s| s.about.describe())));
    let state = match (signed_in, f.connected) {
        (true, _) => format!("signed in to {}", about.unwrap_or_default()),
        (false, true) => "signed out, its music still in the library".into(),
        (false, false) => return "Server: none\n".into(),
    };
    let problem = with(|c| c.problem.as_ref().map(|(p, _)| format!("; {p}")).unwrap_or_default());
    let synced = f.synced_at.map(|at| format!(", synced {}", ago(library::db::now_ms() - at))).unwrap_or_else(|| ", never synced in full".into());
    format!(
        "Server: {state}; tracks {} local only, {} server only, {} both{synced}{problem}\n",
        f.counts.local, f.counts.server, f.counts.both
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_downloaded_cover_updates_visible_cards_and_older_catalog_snapshots() {
        use crate::covers::{self, AlbumModel};
        use library::{Album, ArtCache};
        use slint::{Model, ModelExt};

        let dir = std::env::temp_dir().join(format!("mp-cover-notifications-{}", std::process::id()));
        covers::init(ArtCache::new(&dir));
        let album = Album { id: 42, remote_only: true, remote_art: Some("front".into()), ..Album::default() };
        let local = Album { id: 43, remote_only: false, ..album.clone() };
        let model = AlbumModel::new(vec![album.clone(), local]);
        // Slint re-evaluates this filter only when the model notifies it of a changed row.
        let with_art = model.clone().filter(|card| card.has_cover);
        assert_eq!(with_art.row_count(), 0);
        let written = |key: &str| sync::CoverUpdate { album_id: album.id, remote_art: key.into(), stem: "a10b".into() };
        publish_covers(&[written("previous")]);
        assert_eq!(with_art.row_count(), 0);
        publish_covers(&[written("front")]);
        assert_eq!(with_art.row_count(), 1);
        assert_eq!(with_art.row_data(0).unwrap().id, 42);
        // A fresh search result or grid filter may have captured the album before it was written.
        model.set(vec![album]);
        assert_eq!(model.art_at(0).flatten().as_deref(), Some("a10b"));
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn errors_are_worded_by_their_class() {
        assert!(sign_in_words(&Error::BadUrl, false).contains("not a server address"));
        assert!(sign_in_words(&Error::Network("timed out".into()), false).starts_with("No answer from that address (timed out)"));
        assert!(sign_in_words(&Error::NotSubsonic("a web page or XML".into()), false).contains("/navidrome"));
        assert_eq!(sign_in_words(&Error::WrongCredentials { code: 40, message: "x".into() }, false), "The server did not accept that user name and password.");
        assert!(sign_in_words(&Error::WrongCredentials { code: 41, message: "x".into() }, false).contains("LDAP"));
        assert!(sync_words(&Error::Network("refused".into())).contains("as last synced"));
        assert!(sync_words(&Error::Server { code: None, http_status: Some(503), message: String::new() }).contains("HTTP 503"));
        assert_eq!(class(&Error::Network("https://secret.example/".into())), "no connection", "the log never gets the text");
    }

    #[test]
    fn the_status_names_the_user_the_server_the_counts_and_the_time() {
        let about = About { kind: "navidrome".into(), version: "0.53.3 (x)".into(), api: "1.16.1".into() };
        let f = Facts { counts: remote::Counts { local: 10, server: 1200, both: 980 }, albums: 210, synced_at: Some(1_000_000), connected: true };
        let (title, text) = describe("ann", &about, &f, false, 1_000_000 + 12 * 60_000);
        assert_eq!(title, "Signed in as ann on Navidrome 0.53.3");
        assert_eq!(text, "210 albums, 2,180 tracks on the server; 980 of them also in your folders. Synced 12 min ago.");
        let (_, text) = describe("ann", &about, &Facts::default(), true, 0);
        assert_eq!(text, "Not synced yet. The connection is not encrypted (plain http).");
        assert_eq!((ago(0), ago(3 * 3_600_000 + 5), ago(30 * 3_600_000), ago(5 * 86_400_000)), ("just now".into(), "3 h ago".into(), "yesterday".into(), "5 days ago".into()));
        assert_eq!(describe_step(&Step::Reading { done: 30, total: 120 }).3, "Syncing \u{b7} 25%");
        assert_eq!(sources_line(&remote::Counts { local: 5, server: 0, both: 0 }), "");
        assert_eq!(sources_line(&remote::Counts { local: 5, server: 1200, both: 3 }), "5 only in your folders, 1,200 only on the server, 3 in both");
    }

    #[test]
    fn the_host_is_the_address_without_its_scheme() {
        assert_eq!(host_of("https://music.example.org/nd"), "music.example.org/nd");
        assert_eq!(host_of("http://192.168.1.20:4533/"), "192.168.1.20:4533");
        assert_eq!(host_of("HTTPS://Music.Example.org//"), "Music.Example.org");
        assert_eq!(host_of(" music.example.org "), "music.example.org");
        assert_eq!(host_of("https://"), "");
    }

    #[test]
    fn a_pending_sync_keeps_the_stronger_reason() {
        assert!(Reason::Manual.force() && Reason::SignIn.force() && !Reason::Start.force() && !Reason::Shown.force());
        assert!(!Reason::Start.told() && Reason::Manual.told());
    }
}
