//! The connected Plex server's music sections. Keys stay in the saved session and
//! catalog worker; Settings receives the corresponding names and selected row.

use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{Reason, account, plex::MusicLibrary, with};
use crate::state::State;

pub(super) fn restore(st: &State) {
    let saved = with(|c| c.signed_in.as_ref().and_then(|s| s.account.plex_library()).cloned());
    present(st, saved.into_iter().collect());
}

pub(super) fn present(st: &State, choices: Vec<MusicLibrary>) {
    let names = choices.iter().map(|s| SharedString::from(s.title.as_str())).collect::<Vec<_>>();
    let selected = with(|c| {
        let stored = c.signed_in.as_ref().and_then(|s| s.account.plex_library());
        let selected = stored.and_then(|s| choices.iter().position(|l| l.key == s.key));
        c.plex_libraries = choices;
        selected
    });
    if let Some(app) = st.app.upgrade() {
        let server = app.global::<crate::Server>();
        server.set_plex_libraries(ModelRc::new(VecModel::from(names)));
        server.set_plex_library(selected.map_or(-1, |i| i as i32));
    }
}

// Persist the choice before the first album request. Keep the machine identity so tracks
// present in the chosen section retain their existing row IDs, playlists and play counts.
fn apply(st: &Rc<State>, index: usize) -> bool {
    let result = with(|c| {
        let choice = c.plex_libraries.get(index)?;
        let session = c.signed_in.as_mut()?;
        if c.sync.is_some() || c.signing_out || session.account.plex_library() == Some(choice) {
            return None;
        }
        if session.select_plex_library(choice.clone()).is_err() {
            c.problem = Some(("Could not select that Plex music library. Refresh the list and try again.".into(), false));
            return None;
        }
        c.problem = None;
        c.held = None;
        c.pending = None;
        Some(account::save_plex_selection(&account::path(&st.paths.data), session))
    });
    let Some(saved) = result else { return false };
    if !saved {
        crate::views::flash_long(st, "The music library was selected for this session, but the choice could not be saved.");
    }
    if let Some(app) = st.app.upgrade() {
        app.global::<crate::Server>().set_plex_library(index as i32);
    }
    true
}

pub fn select(st: &Rc<State>, index: i32) {
    if let Ok(index) = usize::try_from(index) && apply(st, index) {
        super::sync(st, Reason::Manual);
    }
}

pub(super) fn choose_or_prompt(st: &Rc<State>, choices: Vec<MusicLibrary>) {
    let was_selected = with(|c| c.signed_in.as_ref().is_some_and(|s| s.account.plex_library().is_some()));
    let count = choices.len();
    present(st, choices);
    // Never silently substitute another section if the saved one disappeared.
    if count == 1 && !was_selected && apply(st, 0) {
        with(|c| c.pending = Some(Reason::SignIn));
        return;
    }
    with(|c| {
        c.held = None;
        c.pending = None;
        c.problem = match (count, was_selected) {
            (0, _) => Some(("This Plex server has no music libraries available to your account.".into(), false)),
            (_, true) => Some(("The selected music library is unavailable. Choose another library below or use Sync now to check again.".into(), false)),
            _ => None,
        };
    });
}
