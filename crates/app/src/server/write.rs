//! What the app writes to the server: favourites both ways, plays reported
//! (docs/features/navidrome.md). Settings > Library > Server's switches decide which kinds may be sent;
//! the client this holds is allowed those kinds only (`subsonic::Writes`), so with both off no
//! write can leave the app, whatever asks. Only `star`, `unstar` and `scrobble`: they change the
//! server's own database (its user's stars and plays), never a music file. No rating, no
//! playlist write, no delete, no tag edit, no scan is ever sent.
//!
//! **One thread** (`start`, for the app's lifetime, idle on a condition variable) does every
//! write in turn, so a change made in the app and a sync's pass never cross:
//! - a favourite changed in the app (`changed`): noted with its time in the database
//!   (`library::marks::touched`) on the UI thread, then sent from here at once. If the server
//!   cannot be reached it is not lost: the local value differs from the agreed one, and the
//!   next sync sends it.
//! - after a sync (`after_sync`): the pass (`pass`) with what the sync read of the server's own
//!   values, which takes the server's changes, sends the app's, and records each agreement. A
//!   mass unstar in what the server said is not taken (`unstar_held`).
//! - a play counted of a track the server has (`plays_queued`): queued in the database with the
//!   play (`query::record_plays_for`), sent from here (`flush_plays`) oldest first, each off the
//!   queue once the server took it; offline, it waits for the next sync.
//! - "now playing" when such a track starts (`now_playing`): sent once, never queued.
//!
//! Signing out, another sign-in and a switch turned off bump the work's generation: what the
//! thread is sending under the older one stops before its next request, unqueueing nothing
//! and writing no agreement (`pass_while`, `flush_plays_while`).
//!
//! The server's play counts are not taken into the library: the app reports its own plays to the
//! server, so taking the server's back would count each of them twice. Ratings are neither read
//! nor sent: one changed in an older build and still waiting there is dropped, never sent
//! (`library::marks`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Condvar, Mutex, OnceLock};

use library::marks::{self, Plan, Row, Seen};
use rusqlite::Connection;
use subsonic::{Client, Error, Writes};

use crate::state::State;

/// What a sync read of the server's own values, by song id (songs not in it: not known this time).
pub type View = HashMap<String, Seen>;

/// Stars sent in one call.
const STAR_CHUNK: usize = 50;
/// Plays read off the queue at a time.
const PLAY_CHUNK: usize = 50;
/// Tracks decided on per transaction by a pass: the first pass of a large library decides on
/// every track, and one transaction for all of them would keep the app's own writes (a play
/// counted) waiting.
const DECIDE_CHUNK: usize = 1000;

/// A local favourite the server's value replaced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub track_id: i64,
    pub favourite: Option<bool>,
}

/// What a pass did.
#[derive(Debug, Default)]
pub struct Done {
    /// Taken from the server.
    pub changed: Vec<Changed>,
    /// Values the server took.
    pub sent: usize,
    /// Transactions the decisions were written in.
    pub batches: usize,
    /// Why sending stopped (the rest goes at the next sync).
    pub error: Option<Error>,
}

/// The server's own values as a sync read them, from its catalog's songs.
pub fn view_of(albums: &[library::remote::RemoteAlbum]) -> View {
    albums.iter().flat_map(|a| &a.songs).map(|s| (s.id.clone(), Seen { starred: Some(s.starred.map(|t| t.max(0))) })).collect()
}

/// The server's stars (`getStarred2`) when its catalog was not listed: every song kept is known
/// starred or not.
pub fn view_of_starred(kept: impl IntoIterator<Item = String>, starred: &[subsonic::Song]) -> View {
    let mut v: View = kept.into_iter().map(|id| (id, Seen { starred: Some(None) })).collect();
    for s in starred {
        // Listed among the stars: starred, whatever its time says.
        let at = crate::server::provider::star_time(s.starred.as_deref()).unwrap_or(0);
        v.insert(s.id.clone(), Seen { starred: Some(Some(at)) });
    }
    v
}

/// More favourites unstarred on the server than this at once (and more than half of those
/// agreed starred) is taken for an answer that lost its stars (a server that did not say them, a
/// listing cut short), not for the user's doing: none of them is taken in that pass.
const UNSTAR_GUARD_MIN: usize = 25;

/// Whether the pass is to take none of the server's unstars (`UNSTAR_GUARD_MIN`): more than 25
/// and more than half of the favourites agreed starred. 25, as the shrink guard of a scan and a
/// sync (`scan::GUARD_MIN_MISSING`): a user unstarring a handful, or a tenth of a large
/// collection, on the server is let through; a server whose answer has no stars at all for a
/// collection of any size is not. Held, the favourites stay here, nothing is sent for them
/// either (the server's value is not pushed back over), and each pass looks again.
fn unstar_held(rows: &[Row], plans: &[Plan<bool>]) -> bool {
    let unstars = plans.iter().filter(|p| p.set_local == Some(false)).count();
    let agreed = rows.iter().filter(|r| r.agreed.starred == Some(true)).count();
    unstars > UNSTAR_GUARD_MIN && unstars * 2 > agreed
}

/// Compare every track the server has (or only `only`) with what the server said (`view`), and
/// act on it (`library::marks`): the server's changes written here first, then the app's sent,
/// each agreement recorded once the server took it. Sending stops at the first error that is not
/// the song's own (no connection, the sign-in refused); what was not sent goes next time. (The
/// thread runs `pass_while`; this, for the tests.)
#[cfg(test)]
pub fn pass(conn: &mut Connection, client: &Client, view: &View, only: Option<i64>) -> rusqlite::Result<Done> {
    pass_while(conn, client, view, only, &|| true)
}

/// `pass`, for as long as `live` says the client is still the one to send with (`Work`'s
/// generation): looked at before each request and inside each agreement's transaction, so once
/// signed out (or the switches changed) nothing more is sent with this client, and no agreement
/// is written after the sign-out's removal forgot them.
fn pass_while(conn: &mut Connection, client: &Client, view: &View, only: Option<i64>, live: &dyn Fn() -> bool) -> rusqlite::Result<Done> {
    let mut done = Done::default();
    let mut star: Vec<(String, bool)> = Vec::new();
    {
        let _w = (only.is_none()).then(crate::server::sync::writer);
        let decide = |r: &Row| marks::decide_star(r.starred, &r.agreed, view.get(&r.song_id).copied().unwrap_or_default().starred);
        // Whether the server's unstars are held is decided on every track at once (read
        // before the writes; each chunk then decides on its tracks as they are by then).
        let held = only.is_none() && {
            let rows = marks::rows(conn)?;
            let plans: Vec<Plan<bool>> = rows.iter().map(decide).collect();
            let held = unstar_held(&rows, &plans);
            if held {
                let n = plans.iter().filter(|p| p.set_local == Some(false)).count();
                log::warn!("server: the server unstarred {n} favourites at once; none of them taken this time (the favourites here sent as ever)");
            }
            held
        };
        let mut after: Option<(String, i64)> = None;
        loop {
            library::db::let_waiting_in();
            // Read inside the write transaction, after any wait for the writer: a change the
            // user made meanwhile is decided on as it is.
            let tx = library::db::write(conn)?;
            if !live() {
                return Ok(done);
            }
            let rows: Vec<Row> = match only {
                Some(t) => marks::row_of(&tx, t)?.into_iter().collect(),
                None => marks::rows_after(&tx, after.as_ref().map(|(s, t)| (s.as_str(), *t)), DECIDE_CHUNK)?,
            };
            let Some(last) = rows.last() else { break };
            after = Some((last.song_id.clone(), last.track_id));
            for r in &rows {
                let s = decide(r);
                if held && s.set_local == Some(false) {
                    continue;
                }
                if let Some(v) = s.set_local {
                    marks::set_local_star(&tx, r.track_id, v, s.at)?;
                    done.changed.push(Changed { track_id: r.track_id, favourite: Some(v) });
                }
                if let Some(v) = s.agree {
                    marks::agree_star(&tx, &r.song_id, v, s.at)?;
                }
                if let Some(v) = s.push {
                    star.push((r.song_id.clone(), v));
                }
            }
            tx.commit()?;
            done.batches += 1;
            if only.is_some() {
                break;
            }
        }
    }
    // Sent, and agreed as each is taken.
    let mut agreed_stars: Vec<(String, bool)> = Vec::new();
    'send: for value in [true, false] {
        let ids: Vec<&str> = star.iter().filter(|(_, v)| *v == value).map(|(id, _)| id.as_str()).collect();
        for chunk in ids.chunks(STAR_CHUNK) {
            let send = |ids: &[&str]| {
                if !live() {
                    return Err(Error::ReadOnly("signed out"));
                }
                if value { client.star(ids, &[], &[]) } else { client.unstar(ids, &[], &[]) }
            };
            match send(chunk) {
                Ok(()) => agreed_stars.extend(chunk.iter().map(|id| ((*id).to_owned(), value))),
                // One of them is gone from the server: each alone, the gone one left out.
                Err(Error::NotFound(_)) => {
                    for id in chunk {
                        match send(&[id]) {
                            Ok(()) => agreed_stars.push(((*id).to_owned(), value)),
                            Err(Error::NotFound(_)) => log::info!("server: a favourite not sent, the song is no longer on the server"),
                            Err(e) => {
                                done.error = Some(e);
                                break 'send;
                            }
                        }
                    }
                }
                Err(e) => {
                    done.error = Some(e);
                    break 'send;
                }
            }
        }
    }
    done.sent = agreed_stars.len();
    if done.sent > 0 {
        let tx = library::db::write(conn)?;
        if !live() {
            return Ok(done);
        }
        for (id, v) in &agreed_stars {
            marks::agree_star(&tx, id, *v, None)?;
        }
        tx.commit()?;
    }
    if let Some(e) = &done.error {
        log::info!("server: {} favourites sent, the rest waits for the next sync ({})", done.sent, crate::server::class(e));
    } else if done.sent > 0 || !done.changed.is_empty() {
        log::info!("server: {} favourites sent, {} taken from the server", done.sent, done.changed.len());
    }
    Ok(done)
}

/// Send the plays queued, oldest first, each off the queue once the server took it (or when the
/// server no longer has its song). How many were sent, and the error that stopped it. (The
/// thread runs `flush_plays_while`; this, for the tests.)
#[cfg(test)]
pub fn flush_plays(conn: &Connection, client: &Client) -> rusqlite::Result<(usize, Option<Error>)> {
    flush_plays_while(conn, client, &|| true)
}

/// `flush_plays`, for as long as `live` says the client is still the one to send with: once it
/// is not, the rest stays queued, unsent, for the next client.
fn flush_plays_while(conn: &Connection, client: &Client, live: &dyn Fn() -> bool) -> rusqlite::Result<(usize, Option<Error>)> {
    let mut sent = 0;
    loop {
        let batch = marks::queued_plays(conn, PLAY_CHUNK)?;
        if batch.is_empty() {
            return Ok((sent, None));
        }
        for p in batch {
            if !live() {
                return Ok((sent, None));
            }
            match client.scrobble(&p.song_id, Some(p.played_at), true) {
                Ok(()) => sent += 1,
                Err(Error::NotFound(_)) => log::info!("server: a play not reported, the song is no longer on the server"),
                Err(e) => {
                    log::info!("server: {sent} plays reported, the rest waits ({})", crate::server::class(&e));
                    return Ok((sent, Some(e)));
                }
            }
            marks::unqueue_play(conn, p.id)?;
        }
    }
}

/// A disconnected Subsonic catalog still queues changes for the next sign-in. Plex is read-only.
fn catalog_supports_writes(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(library::remote::server(conn)?.is_some_and(|s| !super::provider::is_plex_identity(&s.identity)))
}

pub fn reports_plays(settings: &crate::settings::Settings, conn: &Connection) -> rusqlite::Result<bool> {
    Ok(settings.server_plays && catalog_supports_writes(conn)?)
}

/// The library's catalog is this client's server and user (`remote::server`'s identity), so
/// what is sent is about the songs it has.
fn holds_catalog_of(conn: &Connection, client: &Client) -> bool {
    match library::remote::server(conn) {
        Ok(Some(s)) => s.identity == crate::server::identity(client.credentials()),
        Ok(None) => false,
        Err(e) => {
            log::warn!("server writes: {e}");
            false
        }
    }
}

// ---- the thread ----------------------------------------------------------------------------

#[derive(Default)]
struct Work {
    /// The signed-in server's client (read-only as given; the writes are added here).
    client: Option<Client>,
    writes: Writes,
    /// Bumped with every client and every change of the switches: work taken under an older
    /// one stops before its next request or agreement, unqueueing nothing.
    generation: u64,
    /// A pass after a sync, with what it read.
    pass: Option<View>,
    /// Tracks whose favourite the app changed.
    tracks: Vec<i64>,
    plays: bool,
    now_playing: Option<String>,
}

impl Work {
    fn idle(&self) -> bool {
        self.client.is_none() || (self.pass.is_none() && self.tracks.is_empty() && !self.plays && self.now_playing.is_none())
    }
}

struct Shared {
    work: Mutex<Work>,
    ready: Condvar,
}

static WORKER: OnceLock<Shared> = OnceLock::new();

fn lock() -> Option<std::sync::MutexGuard<'static, Work>> {
    WORKER.get().map(|w| w.work.lock().unwrap_or_else(|e| e.into_inner()))
}

fn kick(f: impl FnOnce(&mut Work)) {
    if let Some(mut w) = lock() {
        f(&mut w);
        WORKER.get().expect("started").ready.notify_all();
    }
}

/// Start the thread (once). `changed` is told, from that thread, of the local values the
/// server's replaced.
pub fn start(db: PathBuf, changed: impl Fn(Vec<Changed>) + Send + 'static) {
    if WORKER.set(Shared { work: Mutex::default(), ready: Condvar::new() }).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("mp-server-write".into()).spawn(move || {
        library::background_thread();
        let w = WORKER.get().expect("set above");
        let mut conn: Option<Connection> = None;
        loop {
            let (client, generation, pass, tracks, plays, now) = {
                let mut g = w.work.lock().unwrap_or_else(|e| e.into_inner());
                while g.idle() {
                    g = w.ready.wait(g).unwrap_or_else(|e| e.into_inner());
                }
                let client = g.client.clone().expect("checked").with_writes(Writes { playlists: false, ..g.writes });
                (client, g.generation, g.pass.take(), std::mem::take(&mut g.tracks), std::mem::take(&mut g.plays), g.now_playing.take())
            };
            let live = || lock().is_some_and(|g| g.generation == generation);
            if conn.is_none() {
                match library::db::open(&db) {
                    Ok(c) => conn = Some(c),
                    Err(e) => {
                        log::warn!("server writes: {e}");
                        continue;
                    }
                }
            }
            let conn = conn.as_mut().expect("opened");
            if !holds_catalog_of(conn, &client) {
                // Signed in to another server than the one whose music the library shows (until
                // the user confirms its replacement): its songs are not this server's, so a play
                // sent would come back "not found" and be dropped. Everything waits.
                log::info!("server writes: the library holds another server's music; nothing sent");
                continue;
            }
            let w = client.writes;
            if let Some(id) = now.filter(|_| w.plays && live())
                && let Err(e) = client.scrobble(&id, None, false)
            {
                log::debug!("server: now playing not said ({})", crate::server::class(&e));
            }
            let mut taken = Vec::new();
            if w.marks {
                if let Some(view) = &pass {
                    match pass_while(conn, &client, view, None, &live) {
                        Ok(d) => taken.extend(d.changed),
                        Err(e) => log::warn!("server writes: {e}"),
                    }
                }
                for t in tracks {
                    match pass_while(conn, &client, &View::new(), Some(t), &live) {
                        Ok(d) => taken.extend(d.changed),
                        Err(e) => log::warn!("server writes: {e}"),
                    }
                }
            }
            if w.plays
                && (plays || pass.is_some())
                && let Err(e) = flush_plays_while(conn, &client, &live)
            {
                log::warn!("server writes: {e}");
            }
            if !taken.is_empty() {
                changed(taken);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("server writes: cannot start ({e})");
    }
}

/// The signed-in server's client, or None (signed out): nothing is sent without one.
pub fn set_client(client: Option<Client>) {
    kick(|w| {
        w.generation += 1;
        w.client = client;
        w.pass = None;
        w.tracks.clear();
        w.now_playing = None;
        // Plays queued wait in the database for the next client.
        w.plays = w.client.is_some();
    });
}

/// The kinds of write the switches allow (Settings > Library > Server).
pub fn set_writes(writes: Writes) {
    kick(|w| {
        // A kind turned off stops what is being sent at once (not after the pass or the plays
        // under way); what the switches still allow goes again.
        if (w.writes.marks && !writes.marks) || (w.writes.plays && !writes.plays) {
            w.generation += 1;
            w.plays = true;
        }
        w.writes = Writes { playlists: false, ..writes };
    });
}

/// The writes the settings allow.
pub fn writes_of(s: &crate::settings::Settings) -> Writes {
    Writes { marks: s.server_marks, plays: s.server_plays, playlists: false }
}

/// A sync ended having read `view` of the server's own values: the pass, and the plays queued.
pub fn after_sync(view: View) {
    kick(|w| {
        if w.client.is_some() {
            w.pass = Some(view);
            w.plays = true;
        }
    });
}

/// The app changed track `id`'s favourite (the user did): noted, and sent at once when the
/// server has the track and favourites are synced.
pub fn changed(st: &Rc<State>, id: i64) {
    if id < 0 || !st.settings.borrow().server_marks {
        return;
    }
    let touched = catalog_supports_writes(&st.db).and_then(|supported| {
        if supported { marks::touched(&st.db, id) } else { Ok(None) }
    });
    match touched {
        Ok(Some(_)) => kick(|w| {
            if !w.tracks.contains(&id) {
                w.tracks.push(id);
            }
        }),
        Ok(None) => {}
        Err(e) => log::warn!("server: a change not noted for the server ({e})"),
    }
}

/// A play of a track the server has was queued: send it.
pub fn plays_queued() {
    kick(|w| w.plays = true);
}

/// A track the server has started: "now playing" (when plays are reported).
pub fn now_playing(st: &Rc<State>, track: &library::Track) {
    if !st.settings.borrow().server_plays {
        return;
    }
    if let Some(id) = &track.remote_id {
        let id = id.clone();
        kick(|w| w.now_playing = Some(id));
    }
}

/// The server's values that replaced local ones: every list, the player bar and the smart
/// playlists in step (UI thread).
pub fn apply(st: &Rc<State>, changes: &[Changed]) {
    let playing = crate::views::playing_track_id(st);
    let app = st.app.upgrade();
    for c in changes {
        if let Some(fav) = c.favourite {
            for l in st.lists.all() {
                l.set_favourite(c.track_id, fav);
            }
            if c.track_id == playing
                && let Some(app) = &app
            {
                slint::ComponentHandle::global::<crate::Player>(app).set_favourite(fav);
            }
        }
    }
    crate::playlists::smart::changed(st);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use subsonic::mock::{self, Mock, Trouble};

    use super::*;
    use crate::server::sync::{Job, Outcome, run};

    const WRITES: &[&str] = &["star", "unstar", "setRating", "scrobble", "createPlaylist", "updatePlaylist", "deletePlaylist", "startScan"];

    struct Fixture {
        dir: PathBuf,
        mock: Mock,
        creds: subsonic::Credentials,
    }

    impl Fixture {
        /// Three albums of two songs (`al0-1`, `al0-2`, `al1-1`...), only on the server.
        fn new(name: &str) -> Self {
            Self::with(name, 3)
        }
        /// `albums` albums of two songs.
        fn with(name: &str, albums: u64) -> Self {
            let dir = std::env::temp_dir().join(format!("mp-serverwrite-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let mut lib = mock::Library { last_scan: Some("2026-11-01T00:00:00Z".into()), ..Default::default() };
            lib.albums = (0..albums).map(|i| mock::album(&format!("al{i}"), "Ann", &format!("Record {i}"), 2000, "2026-01-01T00:00:00Z", &[("One", 1, 200, 1000 + i, ""), ("Two", 2, 180, 2000 + i, "")])).collect();
            let mock = Mock::start("ann", "pw", lib).unwrap();
            let (creds, _) = subsonic::sign_in(&mock.url, "ann", "pw").unwrap();
            Fixture { dir, mock, creds }
        }
        fn client(&self, writes: Writes) -> Client {
            Client::new(self.creds.clone(), true).unwrap().with_writes(writes)
        }
        /// A sync (listing the catalog when `force`), and what it read of the server's values.
        fn sync(&self, force: bool, marks: bool) -> View {
            let client = crate::server::provider::Client::Subsonic(self.client(Writes::NONE));
            let job = Job { client, identity: "mock (ann)".into(), db: self.dir.join("library.db"), force, confirm_removal: false, cancel: Arc::default(), marks };
            match run(&job, &|_| {}) {
                Outcome::Synced { seen, .. } | Outcome::UpToDate(seen) => seen,
                o => panic!("{o:?}"),
            }
        }
        fn conn(&self) -> Connection {
            library::db::open(&self.dir.join("library.db")).unwrap()
        }
        fn track(&self, song: &str) -> i64 {
            self.conn().query_row("SELECT id FROM tracks WHERE remote_id = ?1", [song], |r| r.get(0)).unwrap()
        }
        fn local(&self, song: &str) -> bool {
            library::query::is_favourite_track(&self.conn(), self.track(song)).unwrap()
        }
        fn server(&self, song: &str) -> bool {
            let mut out = false;
            self.mock.edit(|l| out = l.song(song).unwrap().starred.is_some());
            out
        }
        /// The app changes a favourite, as `changed` notes it.
        fn star_here(&self, song: &str) {
            let c = self.conn();
            let t = self.track(song);
            library::query::toggle_favourite_track(&c, t).unwrap();
            library::marks::touched(&c, t).unwrap();
        }
        fn writes_sent(&self) -> usize {
            self.mock.requests().iter().filter(|r| WRITES.contains(&r.split(' ').next().unwrap_or(""))).count()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn marks_only() -> Writes {
        Writes { marks: true, ..Writes::NONE }
    }

    /// The first sync takes the union of the stars; later each side's change reaches the
    /// other, and round trips leave every value as it was.
    #[test]
    fn favourites_go_both_ways() {
        let f = Fixture::new("both-ways");
        f.mock.edit(|l| l.song_mut("al0-1").unwrap().starred = Some("2026-05-01T10:00:00Z".into()));
        // The catalog in; then, as a library of before this schema, a favourite of its own.
        f.sync(true, true);
        library::query::toggle_favourite_track(&f.conn(), f.track("al1-1")).unwrap();
        let view = f.sync(true, true);
        let client = f.client(marks_only());
        let d = pass(&mut f.conn(), &client, &view, None).unwrap();
        assert!(d.error.is_none(), "{:?}", d.error);
        assert!(f.local("al0-1"), "starred there: starred here");
        assert!(f.local("al1-1") && f.server("al1-1"), "starred here: starred there");
        assert_eq!((d.changed.len(), d.sent), (1, 1));
        // Nothing changed: a second pass sends nothing and changes nothing.
        let view = f.sync(true, true);
        f.mock.clear_requests();
        let d = pass(&mut f.conn(), &client, &view, None).unwrap();
        assert_eq!((d.sent, d.changed.len(), f.writes_sent()), (0, 0, 0));
        // Changed on the server: taken at the next sync (a listing, or the stars alone).
        f.mock.edit(|l| {
            l.song_mut("al0-1").unwrap().starred = None;
            l.song_mut("al2-1").unwrap().starred = Some("2026-06-01T00:00:00Z".into());
        });
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert_eq!((f.local("al0-1"), f.local("al2-1")), (false, true));
        f.mock.edit(|l| l.song_mut("al2-1").unwrap().starred = None);
        let stars = f.sync(false, true);
        assert!(stars.get("al2-1").is_some_and(|s| s.starred == Some(None)), "not listed again: the stars read alone");
        pass(&mut f.conn(), &client, &stars, None).unwrap();
        assert!(!f.local("al2-1"), "unstarred there with no scan");
        // Changed here: sent at once (the one track).
        f.star_here("al2-2");
        let d = pass(&mut f.conn(), &client, &View::new(), Some(f.track("al2-2"))).unwrap();
        assert_eq!(d.sent, 1);
        assert!(f.server("al2-2"));
        // Every value as both sides have it after the round trips, and a sync after them
        // changes nothing on either side.
        let view = f.sync(true, true);
        f.mock.clear_requests();
        let d = pass(&mut f.conn(), &client, &view, None).unwrap();
        assert_eq!((d.sent, d.changed.len(), f.writes_sent()), (0, 0, 0));
        for song in ["al0-1", "al0-2", "al1-1", "al1-2", "al2-1", "al2-2"] {
            assert_eq!(f.local(song), f.server(song), "{song}");
        }
    }

    /// A first pass over a large library decides a chunk of tracks per transaction (the app's
    /// own writes get in between), and still decides on every track.
    #[test]
    fn a_large_first_pass_is_written_a_chunk_at_a_time() {
        let f = Fixture::with("chunks", 1100);
        f.mock.edit(|l| {
            for i in (0..1100).step_by(10) {
                l.song_mut(&format!("al{i}-2")).unwrap().starred = Some("2026-05-01T10:00:00Z".into());
            }
        });
        let view = f.sync(true, true);
        let d = pass(&mut f.conn(), &f.client(marks_only()), &view, None).unwrap();
        assert_eq!((d.batches, d.changed.len()), (3, 110), "2,200 tracks, 1,000 a transaction");
        let agreed: i64 = f.conn().query_row("SELECT COUNT(*) FROM remote_marks WHERE star_agreed IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(agreed, 2200, "every track decided on");
        assert!(f.local("al1090-2") && !f.local("al1090-1"));
    }

    /// The server's answer with no star where many were agreed (a server that does not say
    /// them, an answer cut short): not one favourite is unstarred here, and nothing is sent
    /// back over the server's. A few unstarred there go as ever.
    #[test]
    fn a_mass_unstar_from_the_server_is_not_taken() {
        let f = Fixture::with("mass-unstar", 20);
        let songs: Vec<String> = (0..20).flat_map(|i| [format!("al{i}-1"), format!("al{i}-2")]).collect();
        f.mock.edit(|l| {
            for id in &songs {
                l.song_mut(id).unwrap().starred = Some("2026-05-01T10:00:00Z".into());
            }
        });
        let client = f.client(marks_only());
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert!(songs.iter().all(|s| f.local(s)), "the 40 stars taken and agreed");
        // Every star gone from the answer: none taken, none sent.
        f.mock.edit(|l| {
            for id in &songs {
                l.song_mut(id).unwrap().starred = None;
            }
        });
        f.mock.clear_requests();
        let d = pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert!(d.changed.is_empty() && d.sent == 0, "{d:?}");
        assert!(songs.iter().all(|s| f.local(s)), "every favourite kept");
        assert_eq!(f.writes_sent(), 0);
        // The stars back, three unstarred there: those three are taken.
        f.mock.edit(|l| {
            for id in &songs[3..] {
                l.song_mut(id).unwrap().starred = Some("2026-05-01T10:00:00Z".into());
            }
        });
        let d = pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert_eq!(d.changed.len(), 3, "{d:?}");
        assert!(songs[..3].iter().all(|s| !f.local(s)) && songs[3..].iter().all(|s| f.local(s)));
    }

    /// A star whose time does not read is a star, read from the album or from the stars alone.
    #[test]
    fn a_star_with_a_time_that_does_not_read_is_a_star() {
        assert_eq!(crate::server::provider::star_time(Some("yesterday")), Some(0));
        assert_eq!(crate::server::provider::star_time(None), None);
        let f = Fixture::new("odd-time");
        f.mock.edit(|l| l.song_mut("al0-1").unwrap().starred = Some("not a time".into()));
        let listed = f.sync(true, true);
        assert_eq!(listed.get("al0-1").map(|s| s.starred), Some(Some(Some(0))));
        let stars = f.sync(false, true);
        assert_eq!(stars.get("al0-1").map(|s| s.starred), Some(Some(Some(0))));
        pass(&mut f.conn(), &f.client(marks_only()), &listed, None).unwrap();
        assert!(f.local("al0-1"));
    }

    /// A rating changed in an older build and not yet sent when it was replaced (the local
    /// rating differs from the one agreed, changed since): never sent, no error, nothing left
    /// waiting; the favourites beside it go as ever.
    #[test]
    fn a_rating_left_waiting_by_an_older_build_is_dropped_not_sent() {
        let f = Fixture::new("old-rating");
        let client = f.client(marks_only());
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        let t = f.track("al0-1");
        {
            let c = f.conn();
            c.execute("INSERT INTO track_ratings VALUES (?1, 4, 1)", [t]).unwrap();
            c.execute(
                "INSERT INTO remote_marks (song_id, rating, rating_agreed, rating_local) VALUES ('al0-1', 2, 1000, 5000)
                 ON CONFLICT(song_id) DO UPDATE SET rating = 2, rating_agreed = 1000, rating_local = 5000",
                [],
            )
            .unwrap();
        }
        f.star_here("al1-1");
        f.mock.clear_requests();
        let d = pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert!(d.error.is_none(), "{:?}", d.error);
        assert_eq!(d.sent, 1, "the favourite only");
        assert!(!f.mock.requests().iter().any(|r| r.starts_with("setRating")), "{:?}", f.mock.requests());
        let d = pass(&mut f.conn(), &client, &View::new(), Some(t)).unwrap();
        assert!(d.error.is_none() && d.sent == 0 && d.changed.is_empty(), "nothing to send for it, now or later: {d:?}");
        assert_eq!(f.writes_sent(), 1);
    }

    /// Both sides changed the same favourite: the later change.
    #[test]
    fn both_changed_the_later_favourite_wins() {
        let f = Fixture::new("both-changed");
        let client = f.client(marks_only());
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        // Starred there, taken here: agreed.
        f.mock.edit(|l| l.song_mut("al0-2").unwrap().starred = Some(mock::iso(library::db::now_ms() - 60_000)));
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert!(f.local("al0-2"));
        // Unstarred here, then starred again there (later): the server's.
        f.star_here("al0-2");
        std::thread::sleep(std::time::Duration::from_millis(20));
        f.mock.edit(|l| l.song_mut("al0-2").unwrap().starred = Some(mock::iso(library::db::now_ms())));
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert_eq!((f.local("al0-2"), f.server("al0-2")), (true, true), "starred again there after it was unstarred here: starred");
        // Starred again there, then unstarred here (later): the app's.
        f.mock.edit(|l| l.song_mut("al0-2").unwrap().starred = Some(mock::iso(library::db::now_ms() - 1_000)));
        f.star_here("al0-2");
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert_eq!((f.local("al0-2"), f.server("al0-2")), (false, false), "unstarred here after: unstarred");
    }

    /// A favourite changed here while the pass waits for a scan's writes, and changed there
    /// too: decided on the value as it is once the pass writes, not as it was before it waited.
    #[test]
    fn a_change_made_while_the_pass_waits_is_not_written_over() {
        let f = Fixture::new("waits");
        let client = f.client(marks_only());
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        f.mock.edit(|l| l.song_mut("al2-1").unwrap().starred = Some(mock::iso(library::db::now_ms() - 60_000)));
        let view = f.sync(true, true);
        let scan = crate::server::sync::writer();
        std::thread::scope(|s| {
            let h = s.spawn(|| pass(&mut f.conn(), &client, &view, None).unwrap());
            // The pass is waiting for the writer by now; the user stars and unstars the track
            // meanwhile (later than the server's star).
            std::thread::sleep(std::time::Duration::from_millis(300));
            f.star_here("al2-1");
            f.star_here("al2-1");
            drop(scan);
            h.join().unwrap();
        });
        assert_eq!((f.local("al2-1"), f.server("al2-1")), (false, false), "the later change, made here, wins");
    }

    /// The server gone: a change made here and the plays wait in the library, and go once it
    /// is back, each play at its own time.
    #[test]
    fn offline_changes_and_plays_wait_and_go_later() {
        let f = Fixture::new("offline");
        let client = f.client(Writes { marks: true, plays: true, playlists: false });
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        f.mock.set_trouble(Trouble::Down);
        f.star_here("al1-1");
        f.star_here("al1-2");
        let d = pass(&mut f.conn(), &client, &View::new(), Some(f.track("al1-1"))).unwrap();
        assert!(matches!(d.error, Some(Error::Network(_))) && d.sent == 0, "{d:?}");
        {
            let c = f.conn();
            let mut plays = vec![(f.track("al1-1"), 1_700_000_000), (f.track("al2-1"), 1_700_000_300)];
            library::query::record_plays_for(&c, &mut plays, true).unwrap();
            let (sent, e) = flush_plays(&c, &client).unwrap();
            assert!(sent == 0 && matches!(e, Some(Error::Network(_))));
            assert_eq!(library::marks::queued_count(&c).unwrap(), 2, "kept");
        }
        // Back: the next sync's pass sends what waited.
        f.mock.set_trouble(Trouble::None);
        let d = pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        assert_eq!(d.sent, 2);
        assert_eq!((f.server("al1-1"), f.server("al1-2")), (true, true));
        let (sent, e) = flush_plays(&f.conn(), &client).unwrap();
        assert!(e.is_none() && sent == 2);
        assert_eq!(library::marks::queued_count(&f.conn()).unwrap(), 0);
        let mut scrobbles = Vec::new();
        f.mock.edit(|l| scrobbles = l.scrobbles.clone());
        assert_eq!(
            scrobbles,
            [
                mock::Scrobbled { id: "al1-1".into(), time: Some(1_700_000_000_000), submission: true },
                mock::Scrobbled { id: "al2-1".into(), time: Some(1_700_000_300_000), submission: true }
            ],
            "each once, oldest first, at its own time"
        );
        // A song gone from the server: its play is dropped, the others go.
        let c = f.conn();
        library::marks::queue_play(&c, f.track("al0-1"), 1).unwrap();
        c.execute("INSERT INTO remote_plays (song_id, played_at) VALUES ('gone', 2000)", []).unwrap();
        library::marks::queue_play(&c, f.track("al0-2"), 3).unwrap();
        assert_eq!(flush_plays(&c, &client).unwrap(), (2, None));
        assert_eq!(library::marks::queued_count(&c).unwrap(), 0);
    }

    /// Work taken under a client that is no longer the one (signed out, another account, a
    /// switch turned off): it stops before its next request, unqueueing no play, and writes no
    /// agreement once stopped (the sign-out's removal must not see them come back).
    #[test]
    fn work_of_an_older_client_stops_and_unqueues_nothing() {
        let f = Fixture::new("generation");
        let client = f.client(Writes { marks: true, plays: true, playlists: false });
        pass(&mut f.conn(), &client, &f.sync(true, true), None).unwrap();
        let c = f.conn();
        for (i, song) in ["al0-1", "al0-2", "al2-1"].into_iter().enumerate() {
            library::marks::queue_play(&c, f.track(song), 100 + i as i64).unwrap();
        }
        let calls = std::cell::Cell::new(0);
        let live = || {
            calls.set(calls.get() + 1);
            calls.get() <= 1
        };
        assert_eq!(flush_plays_while(&c, &client, &live).unwrap(), (1, None));
        assert_eq!(library::marks::queued_count(&c).unwrap(), 2, "the two not sent still queued");
        // A favourite sent, then signed out before its agreement is written.
        f.star_here("al1-1");
        calls.set(0);
        let live = || {
            calls.set(calls.get() + 1);
            calls.get() <= 2
        };
        let d = pass_while(&mut f.conn(), &client, &View::new(), Some(f.track("al1-1")), &live).unwrap();
        assert_eq!(d.sent, 1);
        assert!(f.server("al1-1"));
        let agreed: Option<bool> = c.query_row("SELECT starred FROM remote_marks WHERE song_id = 'al1-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(agreed, Some(false), "the agreement of before: none written once stopped");
        // Stopped from the start: nothing sent, nothing written.
        f.star_here("al1-2");
        f.mock.clear_requests();
        let d = pass_while(&mut f.conn(), &client, &View::new(), Some(f.track("al1-2")), &|| false).unwrap();
        assert_eq!((d.sent, f.writes_sent()), (0, 0));
        assert_eq!(flush_plays_while(&c, &client, &|| false).unwrap(), (0, None));
        assert_eq!(library::marks::queued_count(&c).unwrap(), 2);
    }

    /// Signed in to a server whose music the library does not hold (another server's, until its
    /// replacement is confirmed): the thread sends nothing, so no play is answered "not found"
    /// and dropped.
    #[test]
    fn nothing_is_sent_for_a_catalog_of_another_server() {
        let f = Fixture::new("other-catalog");
        let client = f.client(Writes { marks: true, plays: true, playlists: false });
        f.sync(true, true);
        assert!(!holds_catalog_of(&f.conn(), &client), "the library's catalog is named otherwise");
        library::remote::rename(&mut f.conn(), &crate::server::identity(&f.creds), false).unwrap();
        assert!(holds_catalog_of(&f.conn(), &client));
        library::remote::disconnect(&mut f.conn()).unwrap();
        assert!(!holds_catalog_of(&f.conn(), &client), "no catalog at all");
    }

    /// With both switches off the client is read-only: no write request reaches the server,
    /// whatever changed here, and a sync reads nothing more than before.
    #[test]
    fn switches_off_send_no_write_at_all() {
        let f = Fixture::new("off");
        let off = f.client(Writes::NONE);
        f.mock.edit(|l| l.song_mut("al0-1").unwrap().starred = Some("2026-05-01T10:00:00Z".into()));
        f.sync(true, false);
        f.star_here("al1-1");
        f.star_here("al1-2");
        library::query::record_plays_for(&f.conn(), &mut vec![(f.track("al1-1"), 5)], true).unwrap();
        f.mock.clear_requests();
        let d = pass(&mut f.conn(), &off, &View::new(), None).unwrap();
        assert!(matches!(d.error, Some(Error::ReadOnly(_))), "refused by the guard: {d:?}");
        let (sent, e) = flush_plays(&f.conn(), &off).unwrap();
        assert!(sent == 0 && matches!(e, Some(Error::ReadOnly("scrobble"))));
        assert!(off.scrobble("al0-1", None, false).is_err());
        assert!(f.mock.requests().is_empty(), "no request at all: {:?}", f.mock.requests());
        // A sync with favourites off reads no stars.
        f.sync(false, false);
        assert_eq!(f.mock.requests(), ["getScanStatus"]);
        assert_eq!(f.writes_sent(), 0);
        assert_eq!((f.server("al1-1"), f.server("al1-2")), (false, false), "nothing changed there");
    }
}
