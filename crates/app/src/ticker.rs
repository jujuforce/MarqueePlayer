//! When `playback::tick` runs: the UI's housekeeping (engine events, the position in
//! the player bar, play counting, settings and session saves, the tray).
//!
//! Engine events do not wait for a tick: a thread blocks on the engine's channel and
//! runs a tick on the UI thread for each one (`forward`). The timer is only for what
//! moves by itself, and runs no more often than that needs:
//!
//! - playing, window shown: every 100 ms. On macOS the seek bar can tick once per pixel
//!   (up to 500 ms), with the full rate for visible synced lyrics;
//! - playing, window hidden, minimized or covered: every 500 ms (play counting, session saves);
//! - not playing, window shown: once a second (window placement, saves);
//! - not playing, hidden: not at all.
//!
//! Anything that changes playback or asks for a save calls `kick`, which runs at the
//! full rate for a moment (`SETTLE`), so a pause or a seek is shown promptly whatever
//! the rate was, and a settings change is saved when it is due (`state::SaveDelay`, a
//! second after the last change: within `SETTLE`).
//!
//! "Shown" (`shown`) is also what the visualizer runs by: the window visible, not minimized,
//! and not covered. Covered is the system's word for it (winit's `WindowEvent::Occluded`):
//! macOS says it when no pixel of the window can be seen (behind other windows, on another
//! Space, the display asleep or locked, minimized to the Dock), X11 when it is fully obscured;
//! Windows and Wayland never say it, so there a covered window counts as shown, as before.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use player_core::{Event, PlayState};
use slint::{ComponentHandle, Timer, TimerMode};

use crate::{AppWindow, Lyrics, Nav, Page, Player, playback, state};

const FAST: Duration = Duration::from_millis(100);
const HIDDEN: Duration = Duration::from_millis(500);
const SLOW: Duration = Duration::from_secs(1);
/// After a kick, the full rate for this long (a pause takes effect ~50 ms later).
const SETTLE: Duration = Duration::from_millis(1500);

thread_local! {
    static TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    static FAST_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The system said no pixel of the window can be seen (`occluded`).
    static OCCLUDED: Cell<bool> = const { Cell::new(false) };
}

/// The window is visible, not minimized and not covered (see the module doc).
pub fn shown(w: &slint::Window) -> bool {
    is_shown(w.is_visible(), w.is_minimized(), OCCLUDED.with(Cell::get))
}

fn is_shown(visible: bool, minimized: bool, occluded: bool) -> bool {
    visible && !minimized && !occluded
}

/// The system says the window is covered (`true`) or can be seen again (winit's
/// `WindowEvent::Occluded`, from the window's event hook). Covered, the visualizer stops at
/// once and the ticks slow down from the next one; seen again, both come back at once.
pub fn occluded(occluded: bool) {
    if OCCLUDED.with(|o| o.replace(occluded)) == occluded {
        return;
    }
    log::debug!("window {}", if occluded { "covered: the visualizer stops, ticks slow down" } else { "seen again" });
    if !occluded {
        kick();
    }
    state::with(crate::visualizer::wake);
}

/// Forward the engine's events: returns the receiver the UI drains (`State::events`),
/// and a tick runs on the UI thread as each event arrives.
pub fn forward(engine: crossbeam_channel::Receiver<Event>) -> crossbeam_channel::Receiver<Event> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let spawned = std::thread::Builder::new().name("mp-events".into()).spawn(move || {
        // Ends with the engine (its sender is dropped).
        for ev in engine.iter() {
            if tx.send(ev).is_err() {
                return;
            }
            let _ = slint::invoke_from_event_loop(|| {
                kick();
                state::with(playback::tick);
            });
        }
    });
    if let Err(e) = spawned {
        log::error!("cannot start the engine event thread: {e}");
    }
    rx
}

/// Start the timer. Call once, on the UI thread.
pub fn install() {
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, FAST, || {
        state::with(playback::tick);
        reschedule();
    });
    TIMER.with(|t| *t.borrow_mut() = Some(timer));
    kick();
}

/// Playback or settings changed: tick at the full rate for a moment.
pub fn kick() {
    FAST_UNTIL.with(|f| f.set(Some(Instant::now() + SETTLE)));
    set_interval(Some(FAST));
}

fn set_interval(interval: Option<Duration>) {
    TIMER.with(|t| {
        let t = t.borrow();
        let Some(timer) = t.as_ref() else { return };
        match interval {
            Some(i) => {
                if !timer.running() {
                    timer.restart();
                }
                if timer.interval() != i {
                    timer.set_interval(i);
                }
            }
            None => timer.stop(),
        }
    });
}

/// Pick the rate for what is going on now (see the module doc).
fn reschedule() {
    let settling = FAST_UNTIL.with(|f| f.get().is_some_and(|t| Instant::now() < t));
    let interval = state::with(|st| {
        let app = st.app.upgrade()?;
        rate(settling, st.player.status().state == PlayState::Playing, shown(app.window()), || visible_cadence(&app))
    });
    set_interval(interval.flatten());
}

fn rate(settling: bool, playing: bool, shown: bool, visible_cadence: impl FnOnce() -> Duration) -> Option<Duration> {
    match (settling, playing, shown) {
        (true, _, _) => Some(FAST),
        (false, true, true) => Some(visible_cadence()),
        (false, true, false) => Some(HIDDEN),
        (false, false, true) => Some(SLOW),
        (false, false, false) => None,
    }
}

fn visible_cadence(app: &AppWindow) -> Duration {
    if !cfg!(target_os = "macos") {
        return FAST;
    }
    let lyrics = app.global::<Lyrics>();
    if app.global::<Nav>().get_page() == Page::NowPlaying && lyrics.get_shown() && lyrics.get_synced() {
        FAST
    } else if app.get_bar_visible() {
        progress_interval(app.global::<Player>().get_duration(), app.get_seek_width())
    } else {
        HIDDEN
    }
}

/// A logical pixel of travel is enough to show steady progress. The clock and
/// listening count still update at least twice a second on a long track.
fn progress_interval(seconds: f32, pixels: f32) -> Duration {
    if seconds > 0.0 && pixels > 0.0 {
        Duration::from_millis((1000.0 * seconds / pixels) as u64).clamp(FAST, HIDDEN)
    } else {
        HIDDEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_not_shown_ticks_slowly() {
        let visible = || Duration::from_millis(250);
        assert_eq!(rate(false, true, true, visible), Some(visible()));
        assert_eq!(rate(false, true, false, visible), Some(HIDDEN));
        assert_eq!(rate(false, false, true, visible), Some(SLOW));
        assert_eq!(rate(false, false, false, visible), None);
        assert_eq!(rate(true, false, false, visible), Some(FAST));
    }

    #[test]
    fn progress_follows_the_width_and_duration() {
        assert_eq!(progress_interval(180.0, 600.0), Duration::from_millis(300));
        assert_eq!(progress_interval(6.0, 600.0), FAST);
        assert_eq!(progress_interval(3600.0, 600.0), HIDDEN);
        assert_eq!(progress_interval(0.0, 0.0), HIDDEN);
    }

    #[test]
    fn a_covered_window_is_not_shown() {
        assert!(is_shown(true, false, false));
        assert!(!is_shown(true, false, true), "covered");
        assert!(!is_shown(true, true, false), "minimized");
        assert!(!is_shown(false, false, false), "hidden");
    }
}
