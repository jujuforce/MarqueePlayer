//! The session kept across restarts: the queue (its tracks, the current item, the order
//! shuffle turns back to) and how far into the current track playback was, in
//! library.db (`library::session`); shuffle and repeat in settings.json.
//!
//! On start the queue comes back paused where it was: the current track shows in the
//! player bar at its position, but nothing is handed to the engine, so nothing plays
//! (or makes a sound) until the user presses play, which starts it from that position
//! (`State::resume`, `playback::play_current`).
//!
//! Saving is cheap and bounded. The whole queue is written only when it changed (its
//! revision moved), at most once a second. While playing, the position is updated (one
//! row) every `POSITION_EVERY`, and at once on a pause, a track change or exit, so a
//! crash or power cut loses at most a few seconds. Paused, nothing is written.
//!
//! A play counted (`record_play`) is written at once; one the library cannot take then (a
//! full disk, the database held) is kept and written by a later save, with the time it was
//! counted, once.

use std::rc::Rc;
use std::time::{Duration, Instant};

use player_core::PlayState;
use slint::ComponentHandle;

use crate::Player;
use crate::playback;
use crate::queue::{Queue, Repeat};
use crate::settings::RepeatSetting;
use crate::state::State;

const POSITION_EVERY: Duration = Duration::from_secs(5);
const QUEUE_EVERY: Duration = Duration::from_secs(1);

/// What was last written.
pub struct Saved {
    revision: Option<u64>,
    current: Option<usize>,
    seconds: f64,
    playing: bool,
    position_at: Instant,
    queue_at: Instant,
    /// Plays counted not yet written (track, `db::now` when counted), in order.
    plays: Vec<(i64, i64)>,
}

impl Default for Saved {
    fn default() -> Self {
        let long_ago = Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now);
        Self { revision: None, current: None, seconds: 0.0, playing: false, position_at: long_ago, queue_at: long_ago, plays: Vec::new() }
    }
}

/// A play counted of track `id` (in the library): written now, waiting for a scan's batch as
/// the app's writes do; if it cannot be, the pill says so once and later saves write it.
pub fn record_play(st: &Rc<State>, id: i64) {
    st.session.borrow_mut().plays.push((id, library::db::now()));
    if !save_plays(st, true) {
        crate::views::flash(st, "Could not count the play: the library cannot be written now. It is tried again");
    }
}

/// Write the plays not yet written (`force`: waiting as long as a write of the app's may;
/// otherwise briefly, as a save does). Whether none is left.
fn save_plays(st: &Rc<State>, force: bool) -> bool {
    let mut sv = st.session.borrow_mut();
    if sv.plays.is_empty() {
        return true;
    }
    if !force {
        let _ = library::db::set_wait(&st.db, library::db::Wait::Brief);
    }
    // Plays of tracks the server has are queued for it with the play (`server/write.rs`).
    let to_server = st.settings.borrow().server_plays;
    let result = library::query::record_plays_for(&st.db, &mut sv.plays, to_server);
    let _ = library::db::set_wait(&st.db, library::db::Wait::Long);
    let left = sv.plays.len();
    drop(sv);
    match result {
        // Smart playlists on plays (never played, most played...) follow.
        Ok(n) => {
            if n > 0 {
                crate::playlists::smart::changed(st);
                if to_server {
                    crate::server::write::plays_queued();
                }
            }
        }
        Err(e) if force => log::warn!("recording a play: {e}; {left} kept to try again"),
        Err(e) => log::debug!("recording a play deferred: {e}"),
    }
    left == 0
}

pub fn repeat_from(r: RepeatSetting) -> Repeat {
    match r {
        RepeatSetting::Off => Repeat::Off,
        RepeatSetting::All => Repeat::All,
        RepeatSetting::One => Repeat::One,
    }
}

pub fn repeat_to(r: Repeat) -> RepeatSetting {
    match r {
        Repeat::Off => RepeatSetting::Off,
        Repeat::All => RepeatSetting::All,
        Repeat::One => RepeatSetting::One,
    }
}

/// Bring back the saved queue, paused at the saved position. Tracks no longer in the
/// library are left out (see `library::session::load_queue`).
pub fn restore(st: &Rc<State>) {
    let saved = library::session::load_queue(&st.db).unwrap_or_else(|e| {
        log::warn!("saved queue unreadable ({e}); starting with an empty one");
        Default::default()
    });
    let (shuffle, repeat) = {
        let s = st.settings.borrow();
        (s.shuffle, repeat_from(s.repeat))
    };
    let n = saved.items.len();
    let items = saved.items.into_iter().map(|i| (i.track, i.original)).collect();
    let q = Queue::restore(items, saved.current, shuffle, repeat, playback::seed());
    let current = q.current().cloned();
    let revision = q.revision();
    let index = q.current_index();
    *st.queue.borrow_mut() = q;
    let seconds = current.as_ref().map_or(0.0, |c| saved.seconds.clamp(0.0, c.track.duration.max(0.0)));
    if let Some(item) = &current {
        st.resume.set(Some((item.uid, seconds)));
    }
    *st.session.borrow_mut() =
        Saved { revision: Some(revision), current: index, seconds, playing: false, ..Saved::default() };
    log::info!("session: {n} queued, current {index:?} at {seconds:.1} s, shuffle {shuffle}, repeat {repeat:?}");

    if let Some(app) = st.app.upgrade() {
        let p = app.global::<Player>();
        p.set_shuffle(shuffle);
        p.set_repeat(playback::repeat_index(repeat));
    }
    st.models.queue.invalidate();
    playback::refresh_queue(st);
    playback::refresh_now_playing(st);
    if let Some(item) = current {
        playback::show_position(st, seconds, item.track.duration);
    }
}

/// The current item, seconds into it, and whether it is playing; `None` for a moment
/// when that is not known (the engine is between two tracks).
fn where_now(st: &State) -> Option<(Option<usize>, f64, bool)> {
    let q = st.queue.borrow();
    let Some(item) = q.current() else { return Some((None, 0.0, false)) };
    let index = q.current_index();
    if let Some((uid, secs)) = st.resume.get()
        && uid == item.uid
    {
        return Some((index, secs, false));
    }
    let s = st.player.status();
    match s.token.and_then(|t| st.tokens.borrow().get(&t).copied()) {
        Some(uid) if uid == item.uid => Some((index, s.position, s.state == PlayState::Playing)),
        Some(_) => None,
        None => Some((index, 0.0, false)),
    }
}

/// Save what changed (see the module doc). `force` writes now whatever is pending,
/// waiting for the database as long as it takes (at exit).
pub fn autosave(st: &Rc<State>, force: bool) {
    save_plays(st, force);
    let revision = st.queue.borrow().revision();
    let now = Instant::now();
    let Some((current, seconds, playing)) = where_now(st).or_else(|| {
        // Between tracks at exit: keep what was last known.
        let sv = st.session.borrow();
        force.then_some((st.queue.borrow().current_index(), sv.seconds, false))
    }) else {
        return;
    };
    let mut sv = st.session.borrow_mut();
    let queue_due = sv.revision != Some(revision) && (force || now.duration_since(sv.queue_at) >= QUEUE_EVERY);
    let moved = current != sv.current || (seconds - sv.seconds).abs() >= 0.5;
    let position_due = force
        || current != sv.current
        || playing != sv.playing
        || now.duration_since(sv.position_at) >= POSITION_EVERY;
    if !queue_due && !(moved && position_due) && playing == sv.playing {
        return;
    }
    // A save waits only briefly while a scan writes: one that cannot get in is tried again on
    // the next tick, so the UI never waits on a scan. `db::set_wait`, not `busy_timeout`, which
    // would drop the handler that has the scan give way.
    let _ = library::db::set_wait(&st.db, if force { library::db::Wait::Long } else { library::db::Wait::Brief });
    let result = if queue_due {
        let items = st.queue.borrow().snapshot();
        library::session::save_queue(&st.db, &items, current, seconds)
    } else {
        library::session::save_position(&st.db, current, seconds)
    };
    let _ = library::db::set_wait(&st.db, library::db::Wait::Long);
    match result {
        Ok(()) => {
            if queue_due {
                sv.revision = Some(revision);
                sv.queue_at = now;
            }
            sv.current = current;
            sv.seconds = seconds;
            sv.playing = playing;
            sv.position_at = now;
        }
        // Busy (a scan is writing): the next tick tries again.
        Err(e) => log::debug!("session save deferred: {e}"),
    }
}
