//! Plex browser sign-in. Only server names reach Slint. The selected server then uses
//! the same saved session and library sync as a manually connected server.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::plex::cloud::{Pin, Resource};
use super::{account, with};
use crate::state::{self, State};

#[derive(Default)]
pub(super) struct Login {
    cancelled: Arc<AtomicBool>,
    servers: Vec<Resource>,
}

pub fn start(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let s = app.global::<crate::Server>();
    if !s.get_plex() || with(|c| c.plex_login.is_some() || c.signed_in.is_some() || c.signing_in || c.signing_out) {
        return;
    }
    let client_id = {
        let mut settings = st.settings.borrow_mut();
        if settings.plex_client_id.is_empty() {
            settings.plex_client_id = subsonic::new_salt();
            st.save_settings_soon();
        }
        settings.plex_client_id.clone()
    };
    let login = Login::default();
    let cancelled = login.cancelled.clone();
    let attempt = with(|c| {
        c.attempt += 1;
        c.plex_login = Some(login);
        c.signing_in = true;
        c.attempt
    });
    s.set_error("".into());
    s.set_signing_in(true);
    s.set_plex_auth_status("Opening Plex in your browser…".into());
    spawn(st, attempt, move || {
        let pin = Pin::start(&client_id)?;
        let url = pin.browser_url();
        state::post(move |st| {
            if current(attempt) && let Some(app) = st.app.upgrade() {
                if crate::platform::shell::open_url(&url) {
                    app.global::<crate::Server>().set_plex_auth_status("Finish signing in with Plex in your browser, then return here.".into());
                } else {
                    failed(st, attempt, "Could not open your browser. Check your default browser and try again.");
                }
            }
        });
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if cancelled.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Some(servers) = pin.poll()? {
                state::post(move |st| discovered(st, attempt, servers));
                return Ok(());
            }
        }
    });
}

fn discovered(st: &Rc<State>, attempt: u64, servers: Vec<Resource>) {
    if !current(attempt) {
        return;
    }
    if servers.is_empty() {
        failed(st, attempt, "Plex found no available media servers for this account. Check that your server is running and shared with this account.");
        return;
    }
    let only_one = servers.len() == 1;
    with(|c| {
        c.plex_login.as_mut().unwrap().servers = servers;
        c.signing_in = false;
    });
    if only_one {
        select(st, 0);
    } else {
        show_picker(st);
    }
}

fn show_picker(st: &Rc<State>) {
    let Some(app) = st.app.upgrade() else { return };
    let labels = with(|c| c.plex_login.as_ref().unwrap().servers.iter().map(|s| SharedString::from(s.name.as_str())).collect::<Vec<_>>());
    let s = app.global::<crate::Server>();
    s.set_plex_auth_status("".into());
    s.set_signing_in(false);
    s.set_plex_servers(ModelRc::new(VecModel::from(labels)));
}

pub fn select(st: &Rc<State>, index: i32) {
    let Some(app) = st.app.upgrade() else { return };
    let selected = with(|c| {
        if c.signing_in {
            return None;
        }
        let login = c.plex_login.as_ref()?;
        let resource = login.servers.get(usize::try_from(index).ok()?)?.clone();
        c.signing_in = true;
        Some((c.attempt, resource, login.cancelled.clone()))
    });
    let Some((attempt, resource, cancelled)) = selected else { return };
    let s = app.global::<crate::Server>();
    s.set_plex_servers(ModelRc::default());
    s.set_signing_in(true);
    s.set_error("".into());
    s.set_plex_auth_status(format!("Connecting to {}…", resource.name).into());
    spawn(st, attempt, move || {
        let (client, info, token) = resource.connect(&cancelled)?;
        state::post(move |st| {
            if current(attempt) {
                cancel(st);
                super::start_session(st, account::plex_session(client, info), &token);
            }
        });
        Ok(())
    });
}

fn spawn(st: &Rc<State>, attempt: u64, work: impl FnOnce() -> Result<(), &'static str> + Send + 'static) {
    let thread = std::thread::Builder::new().name("mp-plex-signin".into()).spawn(move || {
        if let Err(message) = work() {
            state::post(move |st| failed(st, attempt, message));
        }
    });
    if thread.is_err() {
        failed(st, attempt, "Could not start Plex sign-in. Try again.");
    }
}

fn current(attempt: u64) -> bool {
    with(|c| c.attempt == attempt && c.plex_login.is_some())
}

fn failed(st: &Rc<State>, attempt: u64, message: &str) {
    if !current(attempt) {
        return;
    }
    if with(|c| c.plex_login.as_ref().is_some_and(|login| !login.servers.is_empty())) {
        with(|c| c.signing_in = false);
        show_picker(st);
    } else {
        cancel(st);
    }
    if let Some(app) = st.app.upgrade() {
        app.global::<crate::Server>().set_error(message.into());
    }
}

pub fn cancel(st: &Rc<State>) {
    let Some(login) = with(|c| {
        let login = c.plex_login.take()?;
        c.attempt += 1;
        c.signing_in = false;
        Some(login)
    }) else { return };
    login.cancelled.store(true, Ordering::Relaxed);
    if let Some(app) = st.app.upgrade() {
        let s = app.global::<crate::Server>();
        s.set_signing_in(false);
        s.set_plex_auth_status("".into());
        s.set_plex_servers(ModelRc::default());
        s.set_error("".into());
    }
}
