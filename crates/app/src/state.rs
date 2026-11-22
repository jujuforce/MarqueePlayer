//! App-wide state, owned by the UI thread.
//!
//! One `State` lives in a thread-local so callbacks coming back from other threads
//! (`slint::invoke_from_event_loop`: scanner progress, media keys, file watching) can
//! reach it without threading `Rc`s through `Send` closures.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use library::ArtCache;
use player_core::Player;
use rusqlite::Connection;

use crate::covers::AlbumModel;
use crate::paths::Paths;
use crate::queue::Queue;
use crate::rows::{QueueModel, TrackModel};
use crate::settings::Settings;
use crate::AppWindow;

/// Bookkeeping for counting a play: a play counts once, after three quarters of the track or
/// four minutes of actual listening (`playback::counts_after`), and is scrobbled after half or
/// four minutes (the Last.fm rule).
#[derive(Debug, Default)]
pub struct Listening {
    pub token: u64,
    pub listened: f64,
    pub last_position: f64,
    /// When the last playing position report arrived (`None` before the first, or while paused).
    pub last_report: Option<std::time::Instant>,
    pub counted: bool,
    /// The play as Last.fm and ListenBrainz will get it (start time, tags), until it is
    /// scrobbled (`scrobble::Play`).
    pub scrobble: Option<crate::connections::scrobble::Play>,
}

/// The track lists currently on screen, by the `ctx` name the UI uses.
pub struct Lists {
    pub album: Rc<TrackModel>,
    pub artist: Rc<TrackModel>,
    pub search: Rc<TrackModel>,
    pub playlist: Rc<TrackModel>,
    /// A search's tracks in full ("See all").
    pub tracks: Rc<TrackModel>,
    /// The open smart playlist's tracks, and the smart playlist editor's preview.
    pub smart: Rc<TrackModel>,
    pub preview: Rc<TrackModel>,
}

impl Lists {
    pub fn by_ctx(&self, ctx: &str) -> Option<&Rc<TrackModel>> {
        match ctx {
            "album" => Some(&self.album),
            "artist" => Some(&self.artist),
            "search" => Some(&self.search),
            "playlist" => Some(&self.playlist),
            "tracks" => Some(&self.tracks),
            "smart" => Some(&self.smart),
            "preview" => Some(&self.preview),
            _ => None,
        }
    }

    pub fn all(&self) -> [&Rc<TrackModel>; 7] {
        [&self.album, &self.artist, &self.search, &self.playlist, &self.tracks, &self.smart, &self.preview]
    }
}

pub struct Models {
    pub home: RefCell<Vec<Rc<AlbumModel>>>,
    pub grid: Rc<AlbumModel>,
    /// The open shelf's full page.
    pub shelf: Rc<AlbumModel>,
    pub artist_albums: Rc<AlbumModel>,
    pub artist_appears: Rc<AlbumModel>,
    pub search_albums: Rc<AlbumModel>,
    /// Artists, Genres and Labels tiles (an `Album` per tile: its name as the title).
    pub artists: Rc<AlbumModel>,
    pub groups: Rc<AlbumModel>,
    pub queue: Rc<QueueModel>,
}

/// The Albums page: every album in the chosen order, each with its folded title and
/// artist for the quick filter, which narrows it without asking the database again.
#[derive(Default)]
pub struct GridState {
    pub all: Vec<library::Album>,
    pub keys: Vec<String>,
    pub filter: String,
}

pub struct State {
    pub app: slint::Weak<AppWindow>,
    pub paths: Paths,
    pub settings: RefCell<Settings>,
    pub settings_dirty: Cell<bool>,
    /// When the changes in `settings_dirty` are to be written (`SaveDelay`).
    pub settings_delay: Cell<SaveDelay>,
    /// settings.json was there but could not be read, and no copy of it could be kept: it is
    /// not written over this run (the user's folders and the rest would be lost with it).
    pub settings_held: bool,
    pub db: Connection,
    pub art: ArtCache,
    pub player: Player,
    pub events: crossbeam_channel::Receiver<player_core::Event>,
    pub queue: Rc<RefCell<Queue>>,
    /// Engine token -> queue item uid: the tokens the engine may still report (`new_token`,
    /// `tracks_started`).
    pub tokens: RefCell<HashMap<u64, u64>>,
    pub next_token: Cell<u64>,
    /// Token handed to the engine as the gapless next track, if any.
    pub requested_next: Cell<Option<(u64, u64)>>,
    pub listening: RefCell<Listening>,
    /// Plays that failed in a row (reset when a track starts): stops a queue of only
    /// unplayable tracks from going round for ever with repeat on.
    pub play_failures: Cell<usize>,
    pub history: RefCell<crate::nav::History>,
    pub lists: Lists,
    pub models: Models,
    pub open_album: Cell<Option<i64>>,
    pub open_playlist: Cell<Option<i64>>,
    /// A library job (scan, or removing folders) is running; see scanning.rs.
    pub scanning: Cell<bool>,
    pub rescan_pending: Cell<bool>,
    /// Paths the folder watcher reported during a running job, for a partial scan after it.
    pub pending_paths: RefCell<Vec<PathBuf>>,
    /// Folders removed from the library, waiting for the running scan to finish.
    pub pending_forget: RefCell<Vec<PathBuf>>,
    /// The next scan removes what the mass-removal guard held back (the user said so).
    pub confirm_removal: Cell<bool>,
    /// The restored queue's current item (uid) and where it starts, until it is first
    /// played: the engine has not been given it yet (session.rs).
    pub resume: Cell<Option<(u64, f64)>>,
    /// What the session autosave last wrote.
    pub session: RefCell<crate::session::Saved>,
    pub searcher: crate::search::Searcher,
    /// The native window (HWND), once it exists; 0 before.
    pub hwnd: Cell<isize>,
    pub grid_loaded: Cell<bool>,
    pub grid: RefCell<GridState>,
    /// The shuffle of each random home shelf, by shelf index: kept until reshuffled, so
    /// coming back to Home (or Back) shows the same albums.
    pub shelf_seeds: RefCell<HashMap<usize, i64>>,
    pub watch: RefCell<Option<library::watch::FolderWatch>>,
    pub media: RefCell<Option<crate::platform::media::Controls>>,
    /// The keys of the remappable shortcuts (`keys/map.rs`), from settings.json.
    pub keymap: RefCell<crate::keys::map::Keymap>,
}

thread_local! {
    static STATE: RefCell<Option<Rc<State>>> = const { RefCell::new(None) };
}

pub fn install(st: Rc<State>) {
    STATE.with(|s| *s.borrow_mut() = Some(st));
}

/// Run `f` with the state, if the app is still alive. UI thread only.
pub fn with<R>(f: impl FnOnce(&Rc<State>) -> R) -> Option<R> {
    let st = STATE.with(|s| s.borrow().clone())?;
    Some(f(&st))
}

/// Run `f` on the UI thread with the state. Callable from any thread.
pub fn post(f: impl FnOnce(&Rc<State>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with(f);
    });
}

pub fn drop_state() {
    STATE.with(|s| s.borrow_mut().take());
}

impl State {
    pub fn new_token(&self, uid: u64) -> u64 {
        let t = self.next_token.get() + 1;
        self.next_token.set(t);
        let mut tokens = self.tokens.borrow_mut();
        tokens.insert(t, uid);
        // Tracks that never start (each refused, one after another) are not pruned by a start.
        prune_tokens(&mut tokens, t.saturating_sub(TOKENS_KEPT));
        t
    }

    /// The track with `token` started: the engine has let go of every older token (it reports
    /// only the track playing, the one it was given to play next and those asked for since,
    /// all newer), so they are forgotten.
    pub fn track_started(&self, token: u64) {
        prune_tokens(&mut self.tokens.borrow_mut(), token);
    }

    /// Saved about a second after the last change (`SaveDelay`), on a tick: a slider dragged
    /// across writes the file once it is let go, not ten times a second. Each change kicks the
    /// ticker (`ticker::kick`), so the tick that writes it comes even with nothing playing.
    pub fn save_settings_soon(&self) {
        self.settings_dirty.set(true);
        let mut d = self.settings_delay.get();
        d.changed(Instant::now());
        self.settings_delay.set(d);
        crate::ticker::kick();
    }

    /// Each tick: write what changed once it is due (`save_settings_soon`). A crash or a power
    /// cut before then loses the changes of that last second or so (at most `SaveDelay::LONGEST`
    /// of a change made over and over); every one before is on the disk.
    pub fn save_settings_when_due(&self) {
        let due = self.settings_dirty.get() && self.settings_delay.get().due(Instant::now());
        self.note_placement(due);
        if due {
            self.write_settings();
        }
    }

    /// Write what changed now, not waiting for it to be due: at exit, when the session ends,
    /// and wherever the file must be current at once.
    pub fn flush_settings(&self) {
        self.note_placement(self.settings_dirty.get());
        self.write_settings();
    }

    /// The window's placement goes with the settings: looked at every few seconds (not on
    /// every tick, so a drag does not rewrite the file), when something else is being saved
    /// anyway (`now`), and at exit. Moved: saved like any other change.
    fn note_placement(&self, now: bool) {
        if let Some(app) = self.app.upgrade()
            && let Some(w) = crate::placement::poll(&app, self.hwnd.get(), now)
            && self.settings.borrow().window != Some(w)
        {
            self.settings.borrow_mut().window = Some(w);
            self.save_settings_soon();
        }
    }

    fn write_settings(&self) {
        self.settings_delay.set(SaveDelay::default());
        if self.settings_held {
            self.settings_dirty.set(false);
            return;
        }
        if self.settings_dirty.replace(false) && !self.settings.borrow().save(&self.paths.settings()) {
            // Said once a run: a read-only data folder or a full disk would say it every time.
            static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let folder = self.paths.config.display().to_string();
                crate::state::post(move |st| {
                    crate::views::flash_long(st, &format!("Settings cannot be saved in {folder}: the folder is read-only or the disk is full"));
                });
            }
        }
    }
}

/// When changed settings are written: `QUIET` after the last change, so a run of changes (a
/// slider dragged, a key held) is one write, but no later than `LONGEST` after the first, so
/// one going on and on is still saved now and then. Settings marked changed without a time
/// (`settings_dirty` set directly) are due at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveDelay {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl SaveDelay {
    pub const QUIET: Duration = Duration::from_secs(1);
    pub const LONGEST: Duration = Duration::from_secs(5);

    pub fn changed(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub fn due(&self, now: Instant) -> bool {
        match (self.first, self.last) {
            (Some(first), Some(last)) => now >= last + Self::QUIET || now >= first + Self::LONGEST,
            _ => true,
        }
    }
}

/// At most this many of the newest tokens are kept even when no track starts.
const TOKENS_KEPT: u64 = 256;

/// Forget the tokens older than `oldest`.
fn prune_tokens(tokens: &mut HashMap<u64, u64>, oldest: u64) {
    tokens.retain(|t, _| *t >= oldest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_older_than_the_track_started_are_forgotten() {
        let mut tokens: HashMap<u64, u64> = (1..=5).map(|t| (t, t * 10)).collect();
        // Token 4 started; 5 was asked for as the next track.
        prune_tokens(&mut tokens, 4);
        let mut kept: Vec<u64> = tokens.keys().copied().collect();
        kept.sort_unstable();
        assert_eq!(kept, vec![4, 5]);
        assert_eq!(tokens[&4], 40);
        // Without a start, only the newest are kept.
        let mut many: HashMap<u64, u64> = (1..=1000).map(|t| (t, 0)).collect();
        prune_tokens(&mut many, 1000 - TOKENS_KEPT);
        assert_eq!(many.len() as u64, TOKENS_KEPT + 1);
    }

    #[test]
    fn settings_changes_are_written_a_second_after_the_last_and_within_five_seconds() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Marked changed without a time: due at once.
        assert!(SaveDelay::default().due(t0));
        // A slider dragged for two seconds, a change every 100 ms: not written while it moves.
        let mut d = SaveDelay::default();
        for n in (0..=2000).step_by(100) {
            d.changed(ms(n));
            assert!(!d.due(ms(n)), "written at {n} ms, mid-drag");
            assert!(!d.due(ms(n + 99)));
        }
        assert!(!d.due(ms(2999)));
        assert!(d.due(ms(3000)), "a second after it was let go");
        // Dragged on and on: still written five seconds after the first change.
        let mut d = SaveDelay::default();
        for n in (0..=10_000).step_by(100) {
            d.changed(ms(n));
            if d.due(ms(n)) {
                assert_eq!(n, 5000);
                return;
            }
        }
        panic!("never written while a change went on and on");
    }
}
