//! Glue between the queue, the engine and the now-playing UI.
//!
//! The engine plays one track and, gaplessly, the one handed to it with `set_next`.
//! `sync_next` keeps that "next" in step with the queue after every change, and on
//! every track start, so the engine always has the following track pre-opened, with how
//! to go into it (gapless, or a crossfade: `crossfade.rs`).
//!
//! A track only the server has is requested through `streaming` (its cached file, or the file
//! growing as it downloads); `sync_next` also tells it which server tracks the queue plays now,
//! next and after (`streaming::follow_queue`), so the next one is fetched as it becomes next.
//! One that cannot be requested (signed out) is skipped with a word in the pill when the queue
//! moves on by itself, and stops playback where it is when it was asked for, or when the one
//! after it fails for the same reason (`refused`): a queue of them is never raced through.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use library::{Track, query};
use player_core::{Event, PlayState, TrackRequest, Transition};
use slint::{ComponentHandle, ModelRc};

use crate::covers::{self, Kind};
use crate::queue::Repeat;
use crate::rows::format_time;
use crate::state::{self, Listening, State};
use crate::{Player, Queue as QueueStore, views};

/// Listening needed before a play counts: three quarters of the track or four minutes,
/// whichever comes first.
fn counts_after(duration: f64) -> f64 {
    (duration * 0.75).clamp(1.0, 240.0)
}

/// The engine's request for a track: its file, or for a track only the server has, what
/// `streaming` gives (`Err`: why it cannot be played now, in words for the pill).
fn request(st: &State, uid: u64, track: &Track) -> Result<TrackRequest, String> {
    let token = st.new_token(uid);
    if track.is_remote() {
        return crate::server::streaming::track_request(st, token, track);
    }
    Ok(TrackRequest::new(token, PathBuf::from(&track.path)))
}

thread_local! {
    /// How the engine was told to go into the requested next track.
    static NEXT_VIA: Cell<Transition> = const { Cell::new(Transition::Gapless) };
    /// The queue moves on by itself (a track ended or could not be played), not by a choice.
    static ADVANCING: Cell<bool> = const { Cell::new(false) };
    /// Why the last track could not be played, until one starts: the same reason twice in a
    /// row stops playback there.
    static LAST_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The track that failed is not to be moved past at the `Ended` that follows.
    static HOLD: Cell<bool> = const { Cell::new(false) };
    /// Tracks refused in a row while the queue moved on by itself: only the first is skipped
    /// (whatever the reasons, so a queue of them on repeat never goes round and round).
    static REFUSED_IN_ROW: Cell<usize> = const { Cell::new(0) };
    /// A server track handed to the engine that has not started yet (its token): shown as
    /// loading, and play/pause pauses it rather than starting it again.
    static STARTING: Cell<Option<u64>> = const { Cell::new(None) };
    /// Counts the waits for a server track's bytes (the pill says so after a second of one).
    static WAITING: Cell<u64> = const { Cell::new(0) };
}

/// Records why a track could not be played; true when the one before failed for the same.
fn failed_again(why: &str) -> bool {
    LAST_FAILURE.with(|l| l.replace(Some(why.to_owned()))).as_deref() == Some(why)
}

/// A track that cannot be requested (`request`'s `Err`): said, then skipped when the queue was
/// moving on by itself, unless the one before failed for the same reason or was refused too;
/// else playback stops here, on this item (play tries it again).
fn refused(st: &Rc<State>, item: &crate::queue::Item, why: &str) {
    log::warn!("cannot play {:?}: {why}", item.track.title);
    views::flash_long(st, &format!("{}: {why}", cannot_play(Some(&item.track.title))));
    let again = failed_again(why);
    let in_row = REFUSED_IN_ROW.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    if ADVANCING.with(Cell::get) && !again && in_row == 1 && st.queue.borrow_mut().skip_next().is_some() {
        play_current(st);
        return;
    }
    stop(st);
}

/// Stop where the queue is: nothing plays, starts (a server track loading) or waits to resume,
/// and the player bar, the media controls and the downloads kept follow.
pub fn stop(st: &Rc<State>) {
    st.player.stop();
    st.resume.set(None);
    st.requested_next.set(None);
    STARTING.with(|s| s.set(None));
    crate::metadata::details::ended();
    crate::server::streaming::follow_queue(st);
    show_loading(st);
    refresh_now_playing(st);
    refresh_queue(st);
}

/// The queue moved on by itself (`Ended`): a server track that cannot be played is skipped.
fn advance(st: &Rc<State>) {
    REFUSED_IN_ROW.with(|c| c.set(0));
    ADVANCING.with(|a| a.set(true));
    play_current(st);
    ADVANCING.with(|a| a.set(false));
}

/// A server track is loading: not started yet, or waiting for its download.
pub fn loading(st: &State) -> bool {
    STARTING.with(Cell::get).is_some() || st.player.status().buffering
}

/// What the pill says after a second of loading.
pub const LOADING: &str = "Loading from the server\u{2026}";

/// Show loading on the player bar, and after a second of it, say so in the pill.
fn show_loading(st: &Rc<State>) {
    let on = loading(st);
    let Some(app) = st.app.upgrade() else { return };
    let p = app.global::<Player>();
    if p.get_buffering() == on {
        return;
    }
    p.set_buffering(on);
    let generation = WAITING.with(|w| {
        w.set(w.get() + 1);
        w.get()
    });
    if on {
        slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
            state::with(|st| {
                if WAITING.with(Cell::get) == generation && loading(st) {
                    views::flash_hold(st, LOADING);
                }
            });
        });
    } else {
        views::flash_clear(st, LOADING);
    }
}

/// Start the queue's current item now. While something plays, with crossfade on (and on
/// skips), it crossfades from what is heard (a skip, a track chosen).
pub fn play_current(st: &Rc<State>) {
    let item = st.queue.borrow().current().cloned();
    let Some(item) = item else {
        crate::metadata::details::ended();
        st.player.stop();
        STARTING.with(|s| s.set(None));
        crate::server::streaming::follow_queue(st);
        show_loading(st);
        refresh_now_playing(st);
        refresh_queue(st);
        return;
    };
    crate::ticker::kick();
    if !ADVANCING.with(Cell::get) {
        // Asked for: a refusal of this one is the first of its row.
        REFUSED_IN_ROW.with(|c| c.set(0));
    }
    let req = match request(st, item.uid, &item.track) {
        Ok(req) => req,
        Err(why) => return refused(st, &item, &why),
    };
    STARTING.with(|s| s.set(item.track.is_remote().then_some(req.token)));
    let playing = st.player.status().state == PlayState::Playing && !st.player.is_paused();
    let skip_fade = crate::crossfade::skip_secs(&st.settings.borrow().crossfade).filter(|_| playing);
    // A restored session starts where it left off (see session.rs).
    match st.resume.take().filter(|(uid, _)| *uid == item.uid) {
        Some((_, secs)) if secs > 0.0 => st.player.play_from(req, secs),
        _ => match skip_fade {
            Some(secs) => st.player.play_crossfade(req, secs),
            None => st.player.play(req),
        },
    }
    st.requested_next.set(None);
    sync_next(st);
    show_loading(st);
    refresh_now_playing(st);
    refresh_queue(st);
}

/// How the engine was told to go into the next track (for the bench).
#[cfg(feature = "bench")]
pub fn next_transition() -> Transition {
    NEXT_VIA.with(Cell::get)
}

/// How the engine is to go from the current item into `next`.
fn transition_to(st: &Rc<State>, next: &crate::queue::Item) -> Transition {
    let cfg = st.settings.borrow().crossfade;
    if !cfg.enabled {
        return Transition::Gapless;
    }
    let Some(cur) = st.queue.borrow().current().cloned() else { return Transition::Gapless };
    let album_next = if cur.track.id > 0 && cur.track.album_id == next.track.album_id && cfg.album_gapless {
        let album = query::album_tracks(&st.db, cur.track.album_id).unwrap_or_default();
        crate::crossfade::album_successor(&cur.track, &next.track, &album)
    } else {
        crate::crossfade::album_successor(&cur.track, &next.track, &[])
    };
    crate::crossfade::transition(&cfg, &cur, next, album_next)
}

/// Hand the engine the item after the current one, if it changed (or how to go into it).
pub fn sync_next(st: &Rc<State>) {
    // The server tracks to fetch follow the queue, whether or not the engine's next changed.
    crate::server::streaming::follow_queue(st);
    let next = st.queue.borrow().peek_next().cloned();
    let wanted = next.as_ref().map(|i| i.uid);
    let via = next.as_ref().map_or(Transition::Gapless, |n| transition_to(st, n));
    if st.requested_next.get().map(|(_, uid)| uid) == wanted && wanted.is_some() && NEXT_VIA.with(Cell::get) == via {
        return;
    }
    match next {
        Some(item) => match request(st, item.uid, &item.track) {
            Ok(req) => {
                st.requested_next.set(Some((req.token, item.uid)));
                NEXT_VIA.with(|v| v.set(via));
                st.player.set_next_with(Some(req), via);
            }
            // Not handed over: the current track ends, and the queue says why when it gets there.
            Err(_) => {
                st.requested_next.set(None);
                st.player.set_next(None);
            }
        },
        None => {
            st.requested_next.set(None);
            st.player.set_next(None);
        }
    }
}

pub fn play_tracks(st: &Rc<State>, tracks: Vec<Track>, start: usize) {
    if tracks.is_empty() {
        return;
    }
    st.queue.borrow_mut().replace(tracks, start);
    play_current(st);
}

pub fn enqueue(st: &Rc<State>, tracks: Vec<Track>, next: bool) {
    let was_empty = st.queue.borrow().current().is_none();
    {
        let mut q = st.queue.borrow_mut();
        let at = if next { q.insert_next(tracks) } else { q.append(tracks) };
        // Nothing current (also with items left, its last one removed): what was added plays.
        if was_empty {
            q.jump(at);
        }
    }
    if was_empty {
        play_current(st);
    } else {
        sync_next(st);
        refresh_queue(st);
    }
}

pub fn toggle_play(st: &Rc<State>) {
    crate::ticker::kick();
    let status = st.player.status();
    // A server track still loading is paused like a playing one, not started again.
    if status.state == PlayState::Stopped && STARTING.with(Cell::get).is_none() {
        // Nothing loaded: start the queue's current item, if any.
        if st.queue.borrow().current().is_some() {
            play_current(st);
        }
        return;
    }
    st.player.set_paused(!st.player.is_paused());
    tick(st);
}

pub fn set_paused(st: &Rc<State>, paused: bool) {
    crate::ticker::kick();
    if st.player.status().state == PlayState::Stopped && STARTING.with(Cell::get).is_none() {
        if !paused {
            toggle_play(st);
        }
        return;
    }
    st.player.set_paused(paused);
}

pub fn next(st: &Rc<State>) {
    if st.queue.borrow_mut().skip_next().is_some() {
        play_current(st);
    }
}

pub fn previous(st: &Rc<State>) {
    // Past the first few seconds, "previous" restarts the track (a restored one stays
    // paused, back at its start).
    if let Some((_, secs)) = st.resume.get()
        && secs > 3.0
        && seek_unloaded(st, 0.0)
    {
        return;
    }
    if st.player.status().position > 3.0 {
        crate::ticker::kick();
        st.player.seek(0.0);
        return;
    }
    if st.queue.borrow_mut().skip_previous().is_some() {
        play_current(st);
    }
}

thread_local! {
    /// When the last scrub seek went out, and the trailing one waiting to go.
    static SCRUB: RefCell<(Option<std::time::Instant>, Option<f64>)> = const { RefCell::new((None, None)) };
}

/// Seeks while the seek bar is dragged: at most one per `SCRUB_EVERY`, and the last
/// position always lands. A seek takes ~15 ms end to end, so the audio keeps up.
const SCRUB_EVERY: std::time::Duration = std::time::Duration::from_millis(50);

/// The restored track, not yet handed to the engine (see session.rs): a seek moves the
/// position it will start from, and shows it. False when the engine has a track.
fn seek_unloaded(st: &Rc<State>, secs: f64) -> bool {
    if st.player.status().token.is_some() {
        return false;
    }
    let Some(item) = st.queue.borrow().current().cloned() else { return false };
    let secs = secs.clamp(0.0, item.track.duration.max(0.0));
    st.resume.set(Some((item.uid, secs)));
    show_position(st, secs, item.track.duration);
    true
}

pub fn seek(st: &Rc<State>, secs: f64) {
    crate::ticker::kick();
    if !seek_unloaded(st, secs) {
        st.player.seek(secs);
    }
    crate::media::seeked(st, secs);
}

/// Position and duration in the player bar.
pub fn show_position(st: &Rc<State>, position: f64, duration: f64) {
    let Some(app) = st.app.upgrade() else { return };
    let p = app.global::<Player>();
    p.set_position(position as f32);
    p.set_duration(duration as f32);
    p.set_position_text(format_time(position).into());
    p.set_duration_text(format_time(duration).into());
}

pub fn scrub(st: &Rc<State>, secs: f64) {
    crate::ticker::kick();
    if seek_unloaded(st, secs) {
        return;
    }
    let now = std::time::Instant::now();
    let wait = SCRUB.with(|s| {
        let mut s = s.borrow_mut();
        match s.0 {
            Some(last) if now.duration_since(last) < SCRUB_EVERY => {
                let first_pending = s.1.is_none();
                s.1 = Some(secs);
                first_pending.then(|| SCRUB_EVERY - now.duration_since(last))
            }
            _ => {
                s.0 = Some(now);
                s.1 = None;
                st.player.seek(secs);
                None
            }
        }
    });
    if let Some(wait) = wait {
        slint::Timer::single_shot(wait, || {
            state::with(|st| {
                let pending = SCRUB.with(|s| {
                    let mut s = s.borrow_mut();
                    s.0 = Some(std::time::Instant::now());
                    s.1.take()
                });
                if let Some(secs) = pending {
                    st.player.seek(secs);
                }
            });
        });
    }
}

pub fn seek_by(st: &Rc<State>, delta: f64) {
    crate::ticker::kick();
    if let Some((_, at)) = st.resume.get()
        && seek_unloaded(st, at + delta)
    {
        return;
    }
    let s = st.player.status();
    if s.token.is_some() {
        let to = (s.position + delta).clamp(0.0, s.duration.unwrap_or(f64::MAX));
        st.player.seek(to);
        crate::media::seeked(st, to);
    }
}

pub fn cycle_repeat(st: &Rc<State>) {
    let r = {
        let mut q = st.queue.borrow_mut();
        q.repeat = q.repeat.cycle();
        q.repeat
    };
    st.settings.borrow_mut().repeat = crate::session::repeat_to(r);
    st.save_settings_soon();
    if let Some(app) = st.app.upgrade() {
        app.global::<Player>().set_repeat(repeat_index(r));
    }
    sync_next(st);
    crate::media::update_options(st);
}

/// The player bar's repeat button state.
pub fn repeat_index(r: Repeat) -> i32 {
    match r {
        Repeat::Off => 0,
        Repeat::All => 1,
        Repeat::One => 2,
    }
}

pub fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
}

/// Shuffle on or off (see `Queue`): the playing track keeps playing either way.
pub fn toggle_shuffle(st: &Rc<State>) {
    let on = !st.queue.borrow().shuffled();
    set_shuffle(st, on);
}

pub fn set_shuffle(st: &Rc<State>, on: bool) {
    st.queue.borrow_mut().set_shuffle(on, seed());
    if st.settings.borrow().shuffle != on {
        st.settings.borrow_mut().shuffle = on;
        st.save_settings_soon();
    }
    if let Some(app) = st.app.upgrade() {
        app.global::<Player>().set_shuffle(on);
    }
    sync_next(st);
    refresh_queue(st);
    crate::media::update_options(st);
}

/// Play an album shuffled: shuffle on, starting from a random track.
pub fn shuffle_tracks(st: &Rc<State>, mut tracks: Vec<Track>) {
    if tracks.is_empty() {
        return;
    }
    let n = tracks.len();
    tracks.rotate_left((seed() % n as u64) as usize);
    set_shuffle(st, true);
    play_tracks(st, tracks, 0);
}

/// Insert tracks so the first lands at `index` (a drop on the queue).
pub fn queue_insert(st: &Rc<State>, index: usize, tracks: Vec<Track>) {
    if tracks.is_empty() {
        return;
    }
    let was_empty = st.queue.borrow().current().is_none();
    {
        let mut q = st.queue.borrow_mut();
        let at = q.insert_at(index, tracks);
        if was_empty {
            q.jump(at);
        }
    }
    if was_empty {
        play_current(st);
    } else {
        sync_next(st);
        refresh_queue(st);
    }
}

/// Move the queue row at `from` to index `to` (after the move). Playback continues.
pub fn queue_move(st: &Rc<State>, from: usize, to: usize) {
    st.queue.borrow_mut().move_item(from, to);
    sync_next(st);
    refresh_queue(st);
}

/// Move the queue rows `rows` as one block to the line before `at` (`Queue::move_block`).
/// Where the block starts now. Playback continues.
pub fn queue_move_block(st: &Rc<State>, rows: &[usize], at: usize) -> usize {
    let start = st.queue.borrow_mut().move_block(rows, at);
    sync_next(st);
    refresh_queue(st);
    start
}

pub fn queue_play(st: &Rc<State>, index: usize) {
    if st.queue.borrow_mut().jump(index).is_some() {
        play_current(st);
    }
}

/// Remove a queue item. The current one: the item taking its place plays if playback
/// was going, and otherwise waits, ready, as a restored session does (paused or
/// stopped stays that way; play starts it from the beginning).
pub fn queue_remove(st: &Rc<State>, index: usize) {
    queue_remove_many(st, &[index]);
}

/// Remove several queue items at once (a selection), the current one as `queue_remove` does.
pub fn queue_remove_many(st: &Rc<State>, rows: &[usize]) {
    let was_current = st.queue.borrow().current_index().is_some_and(|c| rows.contains(&c));
    let playing = st.player.status().state != PlayState::Stopped && !st.player.is_paused();
    st.queue.borrow_mut().remove_many(rows);
    if was_current && playing {
        play_current(st);
    } else if was_current {
        // Also a server track still loading. The engine forgets its next track on stop;
        // play_current hands it over again.
        stop(st);
    } else {
        sync_next(st);
        refresh_queue(st);
    }
}

pub fn queue_clear(st: &Rc<State>) {
    // Keep what is playing; drop everything else.
    let current = st.queue.borrow().current().cloned();
    {
        let mut q = st.queue.borrow_mut();
        q.clear();
        if let Some(c) = current {
            q.append(vec![c.track]);
            q.jump(0);
            // The engine still knows the old uid; map its token to the new one.
            let new_uid = q.current().map(|i| i.uid).unwrap_or(0);
            if let Some((uid, secs)) = st.resume.get()
                && uid == c.uid
            {
                st.resume.set(Some((new_uid, secs)));
            }
            for v in st.tokens.borrow_mut().values_mut() {
                if *v == c.uid {
                    *v = new_uid;
                }
            }
        }
    }
    sync_next(st);
    refresh_queue(st);
}

/// The current track has just ended (a hand-over or the end): if that was well short of its
/// length, the rest of its file could not be read, and the user hears why a song "stopped half
/// way". A crossfade switches at least nine tenths in; an MP3 without a VBR header can be a
/// little shorter than its estimated length: hence 70%, and at least 2 s short.
fn say_if_ended_early(st: &Rc<State>) {
    let at = st.listening.borrow().last_position;
    let gone = st.queue.borrow().current().map(|i| (i.track.title.clone(), i.track.duration));
    if let Some((title, duration)) = gone
        && duration > 0.0
        && at > 0.0
        && at < duration * 0.7
        && duration - at >= 2.0
    {
        log::warn!("{title:?} ended at {at:.1} s of {duration:.1} s: the rest of its file could not be decoded");
        views::flash_long(st, &format!("\u{201c}{title}\u{201d} stopped early: the rest of its file could not be read (it may be damaged or incomplete)"));
    }
}

/// Drain engine events and refresh position. Runs as each engine event arrives and on
/// the ticker's timer, paced to the visible playback controls (ticker.rs).
pub fn tick(st: &Rc<State>) {
    crate::wakes::note(&crate::wakes::TICKS);
    while let Ok(ev) = st.events.try_recv() {
        match ev {
            Event::TrackStarted { token } => {
                // The gapless hand-over: the track before may have stopped well short of its
                // length (`say_if_ended_early`).
                if st.requested_next.get().map(|(t, _)| t) == Some(token) {
                    say_if_ended_early(st);
                }
                crate::metadata::details::track_started(token);
                LAST_FAILURE.with(|l| l.borrow_mut().take());
                HOLD.with(|h| h.set(false));
                REFUSED_IN_ROW.with(|c| c.set(0));
                STARTING.with(|s| s.set(None));
                let uid = st.tokens.borrow().get(&token).copied();
                if let Some(uid) = uid {
                    st.queue.borrow_mut().set_current_uid(uid);
                }
                st.track_started(token);
                if st.requested_next.get().map(|(t, _)| t) == Some(token) {
                    // The gapless hand-over happened; the engine needs the one after.
                    st.requested_next.set(None);
                }
                st.play_failures.set(0);
                sync_next(st);
                refresh_now_playing(st);
                refresh_queue(st);
                let track = st.queue.borrow().current().map(|i| i.track.clone());
                if let Some(t) = &track {
                    crate::server::write::now_playing(st, t);
                }
                let scrobble = track.as_ref().and_then(|t| crate::connections::scrobble::track_started(t, crate::connections::scrobble::allowed(&st.settings.borrow(), t), Some(&st.db)));
                *st.listening.borrow_mut() = Listening { token, scrobble, ..Default::default() };
            }
            Event::Ended => {
                // A track that could not be opened was said already (`Error`); one that ran out
                // of readable audio before its end (reached before its successor was set) here.
                if st.play_failures.get() == 0 {
                    say_if_ended_early(st);
                }
                crate::metadata::details::ended();
                STARTING.with(|s| s.set(None));
                if HOLD.with(|h| h.replace(false)) {
                    // A server track failed for the same reason as the one before (the server
                    // unreachable): playback stops on it rather than trying the whole queue.
                    st.play_failures.set(0);
                    refresh_now_playing(st);
                    continue;
                }
                // Normally the next track is already playing gaplessly. This is the
                // end of the queue, a track too short for the hand-over, or a track
                // that could not be played (the engine reports it, then ends): the one
                // place the queue moves past it.
                let failures = st.play_failures.get();
                let (len, repeat) = {
                    let q = st.queue.borrow();
                    (q.items().len(), q.repeat)
                };
                if repeat != Repeat::Off && failures > 0 && failures >= len {
                    // Every item failed in a row: with repeat on this would go round for
                    // ever. Stop here. (Without repeat the queue simply runs out.)
                    st.play_failures.set(0);
                    views::flash_long(st, "Nothing in the queue could be played");
                    refresh_now_playing(st);
                    continue;
                }
                let more = st.queue.borrow_mut().skip_next().is_some();
                if more {
                    advance(st);
                } else {
                    refresh_now_playing(st);
                }
            }
            Event::Output(info) => crate::output::on_info(st, info),
            Event::NoOutput(why) => crate::output::on_no_output(st, why),
            // A server track waiting for its download: the player bar shows it (`show_loading`,
            // below), the position holds, and listening does not accrue meanwhile.
            Event::Buffering { .. } => {}
            Event::Error { token, message } => {
                // Reported only. The engine moves on by itself (`Event::Error`): the
                // `Ended` it sends after a failed play advances the queue. Skipping here
                // as well skipped the track after the bad one.
                log::warn!("playback error: {message}");
                if st.requested_next.get().is_some_and(|(t, _)| t == token) {
                    // The gapless next track failed to pre-open. It is reported when it
                    // is reached (played, it fails again), not now, while the track
                    // before it is still playing.
                    continue;
                }
                let uid = st.tokens.borrow().get(&token).copied();
                if uid.is_some() && uid == st.queue.borrow().current().map(|i| i.uid) {
                    st.play_failures.set(st.play_failures.get() + 1);
                }
                let track = uid.and_then(|uid| st.queue.borrow().items().iter().find(|i| i.uid == uid).map(|i| i.track.clone()));
                if STARTING.with(Cell::get) == Some(token) {
                    STARTING.with(|s| s.set(None));
                }
                let mut note = cannot_play(track.as_ref().map(|t| t.title.as_str()));
                // A server track's download failed: why, in its words (server unreachable,
                // signed out, the file gone). Twice in a row for the same: stop there.
                if let Some(t) = track.as_ref().filter(|t| t.is_remote()) {
                    let why = crate::server::streaming::failure_words(t, &message);
                    note = format!("{note}: {why}");
                    if uid == st.queue.borrow().current().map(|i| i.uid) && failed_again(&why) {
                        HOLD.with(|h| h.set(true));
                    }
                    views::flash_long(st, &note);
                    continue;
                }
                // When the format is the reason (5.1 AAC), say so: the file is not broken.
                if let Some(why) = track.as_ref().and_then(|t| library::audio::unplayable_format(&t.audio, Some(&message))) {
                    note = format!("{note}: {}", why.split(':').next().unwrap_or(&why));
                }
                views::flash_long(st, &note);
            }
        }
    }

    show_loading(st);
    let s = st.player.status();
    let Some(app) = st.app.upgrade() else { return };
    app.set_window_shown(app.window().is_visible());
    let p = app.global::<Player>();
    // A server track not started yet shows as playing (pause pauses it) unless paused.
    p.set_playing(s.state == PlayState::Playing || (STARTING.with(Cell::get).is_some() && !st.player.is_paused()));
    let mut position = s.position;
    if let Some(starting) = STARTING.with(Cell::get)
        && s.token != Some(starting)
    {
        // A server track not started yet: its own start, not where the one before it was.
        let dur = st.queue.borrow().current().map_or(0.0, |i| i.track.duration);
        show_position(st, 0.0, dur);
        position = 0.0;
    } else if let Some(token) = s.token {
        let dur = s.duration.unwrap_or_else(|| st.queue.borrow().current().map_or(0.0, |i| i.track.duration));
        p.set_position(s.position as f32);
        p.set_duration(dur as f32);
        p.set_position_text(format_time(s.position).into());
        p.set_duration_text(format_time(dur).into());
        count_listening(st, token, s.position, dur, s.state == PlayState::Playing);
        crate::lyrics::tick(st, s.position);
    }
    // A server track loading is paused to the media controls (`media::shown_state`).
    crate::media::update_playback(st, &s, loading(st), position);
    crate::connections::presence::update(st, &s);
    crate::visualizer::wake(st);
    crate::sync_tray();
    st.save_settings_when_due();
    crate::session::autosave(st, false);
}

/// Slack above elapsed wall-clock time when accepting a position step: allows small jitter
/// between the audio clock and UI ticks while still rejecting seeks and crossfade jumps.
const LISTEN_SLACK: f64 = 0.25;

/// One position report of the playing track at `now`: listening accrues from steady forward
/// progress only (not seeks, not the jump to the middle of a crossfade, and not paused spans),
/// bounded by the wall-clock time since the previous playing report so UI stalls or throttled
/// timers still count what actually played. Returns whether the scrobbling rule is met (half
/// or four minutes, longer than 30 s: Last.fm's and ListenBrainz's) and whether the play
/// counts now (once).
fn listen(l: &mut Listening, position: f64, duration: f64, playing: bool, now: std::time::Instant) -> (bool, bool) {
    let step = position - l.last_position;
    l.last_position = position;
    if playing {
        if let Some(prev) = l.last_report {
            let wall = now.saturating_duration_since(prev).as_secs_f64();
            if (0.0..=wall + LISTEN_SLACK).contains(&step) {
                l.listened += step.min(wall);
            }
        }
        l.last_report = Some(now);
    } else {
        l.last_report = None;
    }
    let scrobble = crate::connections::scrobble::counts(duration, l.listened);
    let count = !l.counted && l.listened >= counts_after(duration);
    if count {
        l.counted = true;
    }
    (scrobble, count)
}

/// A position report of track `token`, as `listen` takes it. A report of a track older than the
/// one started last is ignored: the engine's status still names the track before for a moment
/// after `TrackStarted`, and starting afresh there would drop the play to be scrobbled. A newer
/// track (one whose start was not seen) starts a play of its own, with nothing to scrobble.
fn report(l: &mut Listening, token: u64, position: f64, duration: f64, playing: bool, now: std::time::Instant) -> Option<(bool, bool)> {
    if token < l.token {
        return None;
    }
    if token > l.token {
        *l = Listening { token, ..Default::default() };
    }
    Some(listen(l, position, duration, playing, now))
}

fn count_listening(st: &Rc<State>, token: u64, position: f64, duration: f64, playing: bool) {
    let mut l = st.listening.borrow_mut();
    let Some((scrobble, count)) = report(&mut l, token, position, duration, playing, std::time::Instant::now()) else { return };
    if scrobble && let Some(s) = l.scrobble.take() {
        crate::connections::scrobble::scrobble(s);
    }
    if count {
        let uid = st.tokens.borrow().get(&token).copied();
        let track_id = uid.and_then(|uid| st.queue.borrow().items().iter().find(|i| i.uid == uid).map(|i| i.track.id));
        // A file played from outside the library (negative id) records nothing.
        if let Some(id) = track_id.filter(|&id| id > 0) {
            // Kept and tried again if the library cannot take it now.
            crate::session::record_play(st, id);
        }
    }
}

/// The note shown when a track cannot be played.
fn cannot_play(title: Option<&str>) -> String {
    match title.map(str::trim) {
        Some(t) if !t.is_empty() => format!("Can\u{2019}t play \u{201c}{t}\u{201d}"),
        _ => "Can\u{2019}t play this track".to_owned(),
    }
}

/// After a scan: the queued tracks as the library now has them (a moved or renamed file
/// has a new path under the same id; without this the queue would play the old one).
pub fn refresh_queue_tracks(st: &Rc<State>) {
    // One query for the whole queue (a long one after a scan of thousands of files).
    let ids: Vec<i64> = st.queue.borrow().items().iter().map(|i| i.track.id).collect();
    let fresh = match query::tracks_by_id(&st.db, &ids) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("the queue's tracks could not be read again: {e}");
            return;
        }
    };
    let (any, current) = st.queue.borrow_mut().update_tracks(|id| fresh.get(&id).cloned());
    if any {
        st.models.queue.invalidate();
        refresh_queue(st);
    }
    if current {
        refresh_now_playing(st);
    }
}

pub fn refresh_queue(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let store = app.global::<QueueStore>();
    if st.models.queue.refresh() {
        let summary = {
            let q = st.queue.borrow();
            queue_summary(q.items().len(), q.items().iter().map(|i| i.track.duration).sum())
        };
        store.set_rows(ModelRc::from(st.models.queue.clone()));
        store.set_summary(summary.into());
    }
    let current = st.queue.borrow().current_index();
    store.set_current(current.map_or(-1, |i| i as i32));
}

/// "1 track · 4 min", "180 tracks · 32 h 24 min"; "" for an empty queue.
pub fn queue_summary(n: usize, seconds: f64) -> String {
    if n == 0 {
        return String::new();
    }
    let minutes = (seconds / 60.0).round() as u64;
    let length = if minutes >= 60 { format!("{} h {} min", minutes / 60, minutes % 60) } else { format!("{minutes} min") };
    format!("{n} {} · {length}", if n == 1 { "track" } else { "tracks" })
}

pub fn refresh_now_playing(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let p = app.global::<Player>();
    let current = st.queue.borrow().current().cloned();
    let playing_id = current.as_ref().map_or(-1, |i| i.track.id);
    for list in st.lists.all() {
        list.set_playing(playing_id);
    }
    let Some(item) = current else {
        p.set_has_track(false);
        p.set_title("".into());
        p.set_artist("".into());
        p.set_track_id(0);
        p.set_album("".into());
        p.set_cover_stem("".into());
        p.set_cover(Default::default());
        crate::metadata::details::now_playing(st);
        crate::media::update_metadata(st, None);
        crate::lyrics::track_changed(st, None);
        views::update_backdrop(st);
        crate::theme::accent::follow(&st.art, None);
        crate::visualizer::follow_art(st, None);
        crate::sync_tray();
        return;
    };
    let t = &item.track;
    p.set_has_track(true);
    p.set_title(t.title.as_str().into());
    p.set_artist(t.artist.as_str().into());
    p.set_track_id(t.id.max(0) as i32);
    p.set_album(t.album.as_str().into());
    p.set_album_id(t.album_id as i32);
    p.set_favourite(t.id > 0 && query::is_favourite_track(&st.db, t.id).unwrap_or(false));
    crate::theme::accent::follow(&st.art, t.art.clone());
    crate::visualizer::follow_art(st, t.art.clone());
    let stem = t.art.as_deref().unwrap_or_default();
    if p.get_cover_stem().as_str() != stem {
        p.set_cover_stem(stem.into());
        p.set_cover(Default::default());
        if !stem.is_empty() {
            let weak = st.app.clone();
            let wanted = p.get_cover_stem();
            covers::with_image(stem, Kind::Thumb, move |img| {
                if let Some(app) = weak.upgrade() && app.global::<Player>().get_cover_stem() == wanted {
                    app.global::<Player>().set_cover(img);
                }
            });
        }
    }
    crate::media::update_metadata(st, Some(t));
    crate::lyrics::track_changed(st, Some(t));
    views::update_backdrop(st);
    crate::sync_tray();
    crate::metadata::details::now_playing(st);
}

/// The playing track's favourite turned over; its new state, or `None` when it was not (said in
/// the pill).
pub fn toggle_favourite_current(st: &Rc<State>) -> Option<bool> {
    let id = st.queue.borrow().current().map(|i| i.track.id)?;
    if id < 0 {
        views::flash(st, "Not in the library");
        return None;
    }
    let fav = match query::toggle_favourite_track(&st.db, id) {
        Ok(fav) => fav,
        Err(e) => {
            views::not_written(st, "Could not change the favourite", &e);
            return None;
        }
    };
    // The lists, the player bar and the server (server/write.rs) in step.
    crate::selected::favourites_written(st, &[id], fav);
    Some(fav)
}

/// Set the volume (0..1), from the slider or the keys. A change while muted unmutes.
pub fn set_volume(st: &Rc<State>, v: f32) {
    if !v.is_finite() {
        return;
    }
    {
        let mut s = st.settings.borrow_mut();
        s.volume = v;
        s.muted = false;
    }
    st.player.set_volume(v);
    st.save_settings_soon();
    views::refresh_player_settings(st);
    crate::media::update_options(st);
}

/// Step the volume by `steps` of 5% (the volume keys, the wheel over the tray icon), on
/// the 5% grid so up and down retrace each other. Returns the new volume.
pub fn nudge_volume(st: &Rc<State>, steps: i32) -> f32 {
    let v = st.settings.borrow().volume;
    let v = ((v + 0.05 * steps as f32) * 20.0).round() / 20.0;
    set_volume(st, v.clamp(0.0, 1.0));
    st.settings.borrow().volume
}

/// Mute or unmute; the volume is kept to come back to.
pub fn toggle_mute(st: &Rc<State>) {
    let (muted, volume) = {
        let mut s = st.settings.borrow_mut();
        s.muted = !s.muted;
        (s.muted, s.volume)
    };
    st.player.set_volume(if muted { 0.0 } else { volume });
    st.save_settings_soon();
    views::refresh_player_settings(st);
    crate::media::update_options(st);
}

/// The crossfade settings changed: the next track is handed over again with how to go
/// into it now.
pub fn crossfade_changed(st: &Rc<State>) {
    sync_next(st);
}

pub fn apply_gain(st: &Rc<State>) {
    let cfg = st.settings.borrow().gain_config();
    st.player.set_gain_config(cfg);
    // The adjustment shown with the details follows.
    crate::metadata::details::now_playing(st);
}

#[cfg(test)]
mod tests {
    use super::counts_after;

    #[test]
    fn queue_summary_reads_as_a_count() {
        assert_eq!(super::queue_summary(0, 0.0), "");
        assert_eq!(super::queue_summary(1, 238.0), "1 track · 4 min");
        assert_eq!(super::queue_summary(180, 116_640.0), "180 tracks · 32 h 24 min");
    }

    #[test]
    fn unplayable_track_note_names_it() {
        assert_eq!(super::cannot_play(Some("Usseewa")), "Can\u{2019}t play \u{201c}Usseewa\u{201d}");
        assert_eq!(super::cannot_play(Some("  ")), "Can\u{2019}t play this track");
        assert_eq!(super::cannot_play(None), "Can\u{2019}t play this track");
    }

    /// A track measured against its own length through crossfades: it is current from the
    /// middle of the overlap with the one before (it shows from there) to the middle of the
    /// one after, the engine keeping each overlap within a fifth of each track. Every track
    /// is still counted (75% or four minutes) and scrobbled (half or four minutes), with
    /// the longest crossfade, at any length the crossfade applies to.
    #[test]
    fn crossfaded_tracks_are_counted_and_scrobbled_by_their_own_length() {
        for duration in [3.0f64, 10.0, 31.0, 61.0, 180.0, 300.0, 1200.0] {
            let rate = 48_000u32;
            let total = (duration * f64::from(rate)) as u64;
            let len = player_core::crossfade::overlap_frames(12.0, rate, Some(total), total, Some(total)).unwrap_or(0) as f64 / f64::from(rate);
            // Shown from the middle of the incoming overlap to the middle of the outgoing.
            let (from, to) = (len / 2.0, duration - len / 2.0);
            let mut l = super::Listening::default();
            let (mut scrobbled_at, mut counted_at) = (None, None);
            let mut pos = from;
            let mut now = std::time::Instant::now();
            while pos <= to {
                let (s, c) = super::listen(&mut l, pos, duration, true, now);
                if s && scrobbled_at.is_none() {
                    scrobbled_at = Some(pos);
                }
                if c {
                    counted_at = Some(pos);
                }
                pos += 0.1;
                now += std::time::Duration::from_millis(100);
            }
            assert!(counted_at.is_some(), "{duration} s with a {len:.2} s crossfade each side: counted");
            if duration > 30.0 {
                assert!(scrobbled_at.is_some(), "{duration} s: scrobbled");
            }
        }
    }

    /// Each play is scrobbled once: not lost when a position report of the track before comes
    /// just after the next one started (gapless, crossfade, a skip), not twice across seeks
    /// within it, and once again when it is played again (repeat: a new start, a new token).
    #[test]
    fn a_play_is_scrobbled_once_across_track_changes_and_seeks() {
        let t = library::query::Track { title: "Lantern".into(), artist: "A".into(), duration: 200.0, ..Default::default() };
        let play = || crate::connections::scrobble::Play { lastfm: crate::connections::scrobble::scrobble_of(&t, 0), listenbrainz: crate::connections::listens::listen_of(&t, 0, None) };
        let mut scrobbled = Vec::new();
        let mut now = std::time::Instant::now();
        let mut tick = |l: &mut super::Listening, token: u64, pos: f64, advance: std::time::Duration| {
            now += advance;
            if let Some((true, _)) = super::report(l, token, pos, 200.0, true, now)
                && l.scrobble.take().is_some()
            {
                scrobbled.push(token);
            }
        };
        for token in 1..=3u64 {
            // `TrackStarted`, then a report still of the track before, then the track itself.
            let mut l = super::Listening { token, scrobble: Some(play()), ..Default::default() };
            tick(&mut l, token - 1, 199.0, std::time::Duration::from_millis(100));
            let mut pos = 0.0;
            while pos < 200.0 {
                tick(&mut l, token, pos, std::time::Duration::from_millis(500));
                pos += if (50.0..50.5).contains(&pos) { 60.0 } else { 0.5 };
                if (150.0..150.5).contains(&pos) {
                    // A seek back to the start, and on.
                    tick(&mut l, token, 0.0, std::time::Duration::from_millis(100));
                    tick(&mut l, token, 160.0, std::time::Duration::from_millis(100));
                }
            }
        }
        assert_eq!(scrobbled, [1, 2, 3]);
    }

    #[test]
    fn listening_counts_steady_steps_and_stalls_but_rejects_seeks_crossfades_and_pauses() {
        use std::time::{Duration, Instant};

        // Steady 100 ms steps (visible window rate).
        let mut l = super::Listening::default();
        let mut now = Instant::now();
        for i in 0..=100 {
            super::listen(&mut l, i as f64 * 0.1, 200.0, true, now);
            now += Duration::from_millis(100);
        }
        assert!((l.listened - 10.0).abs() < 1e-9, "100 ms steps: {}", l.listened);

        // Steady 500 ms steps (hidden window rate).
        let mut l = super::Listening::default();
        let mut now = Instant::now();
        for i in 0..=20 {
            super::listen(&mut l, i as f64 * 0.5, 200.0, true, now);
            now += Duration::from_millis(500);
        }
        assert!((l.listened - 10.0).abs() < 1e-9, "500 ms steps: {}", l.listened);

        // A 3 s stall: counts what actually played, capped at wall-clock time.
        let mut l = super::Listening::default();
        let t0 = Instant::now();
        super::listen(&mut l, 10.0, 200.0, true, t0);
        super::listen(&mut l, 13.0, 200.0, true, t0 + Duration::from_secs(3));
        assert!((l.listened - 3.0).abs() < 1e-9, "3 s stall: {}", l.listened);
        // Slightly ahead of wall clock within slack: still counts, capped at wall time.
        super::listen(&mut l, 16.15, 200.0, true, t0 + Duration::from_secs(6));
        assert!((l.listened - 6.0).abs() < 1e-9, "capped at wall time: {}", l.listened);

        // A 30 s forward seek: counts 0.
        let mut l = super::Listening::default();
        let t0 = Instant::now();
        super::listen(&mut l, 10.0, 200.0, true, t0);
        super::listen(&mut l, 40.0, 200.0, true, t0 + Duration::from_millis(100));
        assert_eq!(l.listened, 0.0, "30 s forward seek must not count");

        // Backward seek: counts 0, and steady progress after it counts normally.
        let mut l = super::Listening::default();
        let t0 = Instant::now();
        super::listen(&mut l, 50.0, 200.0, true, t0);
        super::listen(&mut l, 50.5, 200.0, true, t0 + Duration::from_millis(500));
        super::listen(&mut l, 20.0, 200.0, true, t0 + Duration::from_millis(600));
        assert!((l.listened - 0.5).abs() < 1e-9, "backward seek must not count");
        super::listen(&mut l, 20.5, 200.0, true, t0 + Duration::from_millis(1100));
        assert!((l.listened - 1.0).abs() < 1e-9, "progress after backward seek counts");

        // Crossfade jump: first report mid-track counts 0, and a jump from 0 to 6 s in 100 ms counts 0.
        let mut l = super::Listening::default();
        let t0 = Instant::now();
        super::listen(&mut l, 6.0, 200.0, true, t0);
        assert_eq!(l.listened, 0.0, "first report mid-crossfade counts 0");
        super::listen(&mut l, 6.1, 200.0, true, t0 + Duration::from_millis(100));
        assert!((l.listened - 0.1).abs() < 1e-9);

        let mut l = super::Listening::default();
        super::listen(&mut l, 0.0, 200.0, true, t0);
        super::listen(&mut l, 6.0, 200.0, true, t0 + Duration::from_millis(100));
        assert_eq!(l.listened, 0.0, "crossfade jump from 0 counts 0");

        // Pause and resume: nothing counted while paused, and the first report after resume
        // does not count the paused span (even if position moved while paused or right at resume).
        let mut l = super::Listening::default();
        let t0 = Instant::now();
        super::listen(&mut l, 10.0, 200.0, true, t0);
        super::listen(&mut l, 10.5, 200.0, true, t0 + Duration::from_millis(500));
        assert!((l.listened - 0.5).abs() < 1e-9);
        // Paused for 20 s (with a report while paused, plus a position jump).
        super::listen(&mut l, 10.5, 200.0, false, t0 + Duration::from_secs(1));
        super::listen(&mut l, 15.0, 200.0, false, t0 + Duration::from_secs(10));
        assert!((l.listened - 0.5).abs() < 1e-9, "nothing counted while paused");
        // Resumed 10 s later: even if position advanced by 5 s since the last paused report,
        // the first report after resume only anchors the clock and counts nothing.
        super::listen(&mut l, 20.0, 200.0, true, t0 + Duration::from_secs(20));
        assert!((l.listened - 0.5).abs() < 1e-9, "first report after resume must not count paused span");
        // Subsequent steady playback counts normally.
        super::listen(&mut l, 20.5, 200.0, true, t0 + Duration::from_millis(20_500));
        assert!((l.listened - 1.0).abs() < 1e-9, "steady playback after resume counts");
    }

    #[test]
    fn play_counting_threshold() {
        assert_eq!(counts_after(200.0), 150.0);
        assert_eq!(counts_after(1200.0), 240.0);
        assert_eq!(counts_after(0.5), 1.0);
    }
}
