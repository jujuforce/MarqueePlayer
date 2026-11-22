//! The server's catalog into the library, and its covers into the art cache: the work, off the
//! UI thread; `server/mod.rs` decides when, and words what happened.
//!
//! **A sync** (`run`, on a thread of its own with its own database connection):
//! 1. `getScanStatus`. Unless asked to list anyway (`Job::force`: sign-in, Sync now, a removal
//!    confirmed), a server that has not scanned since the last sync that saw its whole catalog
//!    is not listed again: nothing on it can have changed (`UpToDate`). With favourites
//!    synced, its stars are read then (`getStarred2`, one request): a star changed on
//!    the server needs no scan of its own.
//! 2. The albums, `getAlbumList2` by name, 500 a page (the most Navidrome gives), until a page
//!    comes back short (or brings no album not listed already, or `MAX_PAGES`: then the listing
//!    is not complete); then each album with its songs (`getAlbum`), three at a time: gentle on a
//!    home server, three times faster than one by one. (`search3` with an empty query would list
//!    songs faster, but whether every server answers it with everything is not known; the album
//!    route is the one the Subsonic API documents for every server.)
//! 3. Every `CHUNK` albums read, they are written (`remote::sync` of what was read, not
//!    `complete`): the first albums are in the library within seconds of signing in, and the
//!    open page takes them in as it does a scan's batches. At the end, the whole catalog once
//!    more, `complete` only when every page and every album was read without an error; only then
//!    is what the server no longer lists removed (and only past the shrink guard: `Held`, which
//!    Settings asks to confirm). Albums written already are not written again (their entries'
//!    hashes match). Then `remote::reconcile`.
//!
//! **Signed in under another name** (the server's other address, http for https, another port,
//! the user spelt otherwise, another user): the library's catalog is kept under the identity it
//! was synced with. Once the albums are listed (before anything is written), their ids are
//! compared with the catalog's (`same_server`): the same server's catalog is kept, under the new
//! name (another user's agreements on favourites forgotten: `remote::rename`); another server's
//! stays as it is, nothing written, until the user confirms its replacement in Settings (the
//! shrink guard's confirmation: `Outcome::OtherServer`, then `Job::confirm_removal`).
//!
//! Writes wait for a running scan, and a scan for a sync's write (`WRITER`): each writes in short
//! transactions that let the app's own writes in (`db::let_waiting_in`), but two background
//! writers taking turns could keep the UI thread's write (a play counted) waiting. Stop is
//! looked at between pages, albums and batches; what was written stays.
//!
//! **Covers** (`covers_*`, one thread for the app's lifetime, idle on a condition variable): an
//! album only the server has (its folder is no folder) has no art of its own; its cover is
//! fetched (`getCoverArt` at `art::FULL_PX`) into the art cache, which makes the same sizes,
//! backdrop and accent as for a local cover. Newest albums first, three at a time; an album on
//! screen without one first of all (`covers_want`). An album with a file of its own keeps the
//! art the scan found for it. What was fetched is recorded in `albums.art_checked` as
//! `server:<cache format>:<cover id>`, a cover the server does not have the same way: so a pass
//! interrupted (quit, the server gone) goes on where it was, a cover the server changes is
//! fetched again, and when the art cache's format changes (`art::CACHE_VERSION`) every server
//! cover is fetched again once, newest first, the old images showing until then (the scan's own
//! re-making never reaches them: they have no file to be made from).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::Duration;

use library::ArtCache;
use library::remote::{self, Catalog, RemoteAlbum, RemoteArtist, RemoteSong, SyncOptions, SyncReport};
use subsonic::{AlbumList, Client, Error};

use crate::server::write::View;

/// Albums per `getAlbumList2` page (Navidrome's most).
const PAGE: u32 = 500;
/// Pages listed at most (a million albums): a server that pages wrongly is not listed forever.
const MAX_PAGES: usize = 2000;
/// Albums read at once (`getAlbum`).
const READERS: usize = 3;
/// Albums read, then written, at a time while listing.
const CHUNK: usize = 60;
/// Covers fetched at once.
const COVER_READERS: usize = 3;

/// Held by whoever writes the library in the background: a scan, folder removal, a sync, a
/// sign-out's removal (see the module doc).
pub static WRITER: Mutex<()> = Mutex::new(());

/// `WRITER`, for a scan (it waits as long as it takes).
pub fn writer() -> MutexGuard<'static, ()> {
    WRITER.lock().unwrap_or_else(|e| e.into_inner())
}

/// What one sync is to do.
pub struct Job {
    pub client: Client,
    /// The server and user (`remote::sync`'s identity). A catalog kept under another identity is
    /// this server's under another name (kept, renamed), or another server's: replaced only when
    /// `confirm_removal` (see `same_server`).
    pub identity: String,
    pub db: PathBuf,
    pub force: bool,
    pub confirm_removal: bool,
    pub cancel: Arc<AtomicBool>,
    /// Favourites are synced with the server: a sync that does not list the catalog
    /// reads the server's stars (`getStarred2`, a read) for the pass that follows.
    pub marks: bool,
}

/// How far a sync is.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// The server's catalog of another server or user is being removed.
    Replacing,
    Asking,
    /// Album list pages read: this many albums so far.
    Listing { albums: usize },
    /// Albums read with their songs, of all listed.
    Reading { done: usize, total: usize },
    /// A scan is writing; the sync waits for it.
    Waiting,
    /// What was read so far is in the library (the open page can take it in).
    Written,
    /// The whole catalog, once more: what the server no longer lists, and the tracks matched
    /// with the folders'.
    Finishing,
}

#[derive(Debug)]
pub enum Outcome {
    /// The server has not scanned since the last complete sync: nothing listed. What it says of
    /// its stars now (empty when not asked).
    UpToDate(View),
    Synced {
        report: SyncReport,
        /// The server's own values of the songs read (`server::write::pass`).
        seen: View,
        /// Albums that could not be read (the listing is then not complete).
        failed: usize,
        complete: bool,
    },
    /// The library holds another server's music, and this server's catalog is not it: nothing
    /// was written; it is replaced only once confirmed (`Job::confirm_removal`).
    OtherServer(Elsewhere),
    /// Stop: what was written stays.
    Stopped,
    /// The server could not be asked (no connection, the sign-in refused...).
    Server(Error),
    /// The library could not be written.
    Library(String),
}

/// Wait for `WRITER`, saying so once, and giving up if stopped.
fn wait_writer(cancel: &AtomicBool, tell: &(dyn Fn(Step) + Sync)) -> Option<MutexGuard<'static, ()>> {
    let mut said = false;
    loop {
        // Stopped first: a sync stopped by signing out must not write again (the sign-out's
        // removal may be waiting for this very lock).
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match WRITER.try_lock() {
            Ok(g) => return Some(g),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if !said {
                    tell(Step::Waiting);
                    said = true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Each album of `ids` with its songs, `READERS` at a time, in order; None for those not read
/// because the sync was stopped.
fn read_albums(client: &Client, ids: &[&str], cancel: &AtomicBool) -> Vec<Option<Result<subsonic::Album, Error>>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Result<subsonic::Album, Error>>>> = Mutex::new((0..ids.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..READERS.min(ids.len()) {
            s.spawn(|| {
                library::background_thread();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= ids.len() || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let r = client.album(ids[i]);
                    out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                }
            });
        }
    });
    out.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// An album that came with far fewer songs than the server counts for it (none, or under half):
/// an answer cut short, not an album emptied. Taken as read, its songs would count as gone.
fn cut_short(a: &subsonic::Album, listed: &subsonic::Album) -> bool {
    let count = a.song_count.max(listed.song_count) as usize;
    count > 0 && a.song.len() * 2 < count
}

fn db_error(e: impl std::fmt::Display) -> Outcome {
    Outcome::Library(e.to_string())
}

/// A sync's error, worded without the identity another server's names (its address and user).
fn sync_error(e: remote::SyncError) -> Outcome {
    match e {
        remote::SyncError::Db(e) => db_error(e),
        remote::SyncError::OtherServer { .. } => Outcome::Library("the library holds another server's music; sign out of it first".into()),
    }
}

/// One sync (see the module doc). `tell` is called from this thread.
pub fn run(job: &Job, tell: &(dyn Fn(Step) + Sync)) -> Outcome {
    let cancel = &*job.cancel;
    let mut conn = match library::db::open(&job.db) {
        Ok(c) => c,
        Err(e) => return db_error(e),
    };
    // The library holds the catalog of a server signed in to under another name: the same server
    // (its other address, the user spelt otherwise) or another. Which one is decided once its
    // albums are listed (`same_server`); nothing is written before.
    let earlier = match remote::server(&conn) {
        Ok(Some(s)) if s.identity != job.identity => Some(s.identity),
        Ok(_) => None,
        Err(e) => return db_error(e),
    };
    let mut replaced = false;
    tell(Step::Asking);
    let status = match job.client.scan_status() {
        Ok(s) => s,
        Err(e) => return Outcome::Server(e),
    };
    if !job.force
        && earlier.is_none()
        && let Ok(Some(s)) = remote::server(&conn)
        && s.synced_at.is_some()
        && s.sync_started.is_none()
        && s.server_scan.is_some()
        && s.server_scan == status.last_scan
    {
        return match up_to_date_view(job, &conn) {
            Ok(v) => Outcome::UpToDate(v),
            Err(e) => Outcome::Server(e),
        };
    }
    // The album list, page by page. Listed while the server scans, it may shift under the
    // pages (an album gone or renamed before the next page moves another past it, unlisted):
    // complete only if the server neither scans now nor scanned meanwhile.
    // One album may be listed twice when the list moved under the pages: once is enough. A page
    // that brings no album not listed already ends the listing: a server that ignores `offset`
    // (each page its first) would otherwise be asked forever.
    let mut listed: Vec<subsonic::Album> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut complete = !status.scanning;
    let mut offset: u32 = 0;
    for pages in 1.. {
        if cancel.load(Ordering::Relaxed) {
            return Outcome::Stopped;
        }
        match job.client.album_list2(&AlbumList::AlphabeticalByName, PAGE, offset, None) {
            Ok(page) => {
                let n = page.len();
                offset = offset.saturating_add(n as u32);
                let before = listed.len();
                listed.extend(page.into_iter().filter(|a| seen.insert(a.id.clone())));
                tell(Step::Listing { albums: listed.len() });
                if n < PAGE as usize {
                    break;
                }
                if listed.len() == before || pages >= MAX_PAGES {
                    log::warn!("server sync: the album list pages no further (after {} albums); nothing is removed this time", listed.len());
                    complete = false;
                    break;
                }
            }
            Err(e) if listed.is_empty() || is_sign_in(&e) => return Outcome::Server(e),
            Err(e) => {
                log::warn!("server sync: the album list stopped after {} albums ({e})", listed.len());
                complete = false;
                break;
            }
        }
    }
    if let Some(earlier) = &earlier {
        let kin = match same_server(&job.client, &conn, &listed, cancel) {
            Ok(k) => k,
            Err(o) => return *o,
        };
        match kin {
            Kin::Same => {
                let other_user = !user_of(earlier).eq_ignore_ascii_case(user_of(&job.identity));
                log::info!("server sync: the server in the library, signed in to under another {}; its music kept", if other_user { "user" } else { "address" });
                let Some(_w) = wait_writer(cancel, tell) else { return Outcome::Stopped };
                if let Err(e) = remote::rename(&mut conn, &job.identity, other_user) {
                    return db_error(e);
                }
            }
            Kin::Other(held) if !job.confirm_removal => {
                log::info!("server sync: another server's music is in the library ({} songs); kept until confirmed", held.songs);
                return Outcome::OtherServer(held);
            }
            Kin::Other(_) | Kin::Nothing => {
                tell(Step::Replacing);
                let Some(_w) = wait_writer(cancel, tell) else { return Outcome::Stopped };
                if let Err(e) = remote::disconnect(&mut conn) {
                    return db_error(e);
                }
                replaced = true;
            }
        }
    }
    let total = listed.len();
    let mut catalog: Vec<RemoteAlbum> = Vec::with_capacity(total);
    let mut failed = 0;
    let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
    // What the chunks wrote (the last pass then finds those entries unchanged).
    let mut wrote = SyncReport::default();
    let opts = SyncOptions { cancel: Some(job.cancel.clone()), ..SyncOptions::default() };
    for (n, chunk) in listed.chunks(CHUNK).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Outcome::Stopped;
        }
        let ids: Vec<&str> = chunk.iter().map(|a| a.id.as_str()).collect();
        let mut part: Vec<RemoteAlbum> = Vec::with_capacity(chunk.len());
        let mut last_error = None;
        for (r, listed) in read_albums(&job.client, &ids, cancel).into_iter().zip(chunk) {
            match r {
                None => return Outcome::Stopped,
                Some(Ok(a)) if cut_short(&a, listed) => {
                    log::info!("server sync: an album came with {} of its {} songs; read as failed", a.song.len(), a.song_count.max(listed.song_count));
                    failed += 1;
                    complete = false;
                }
                Some(Ok(a)) => {
                    // A song listed under two albums goes with the first (in the listing's
                    // order, the same at every sync), also across chunks written apart.
                    let mut album = album_of(&a);
                    album.songs.retain(|s| claimed.insert(s.id.clone()));
                    part.push(album);
                }
                // Removed on the server since it was listed: rightly not in the catalog.
                Some(Err(Error::NotFound(_))) => {}
                Some(Err(e)) if is_sign_in(&e) => return Outcome::Server(e),
                Some(Err(e)) => {
                    failed += 1;
                    complete = false;
                    last_error = Some(e);
                }
            }
        }
        if part.is_empty()
            && let Some(e) = last_error.filter(Error::is_retryable)
        {
            // Nothing of this chunk came: the server went away. What was written stays.
            return Outcome::Server(e);
        }
        if !part.is_empty() {
            let Some(_w) = wait_writer(cancel, tell) else { return Outcome::Stopped };
            let c = Catalog { albums: part.clone(), complete: false, server_scan: None };
            match remote::sync(&mut conn, &job.identity, &c, &opts) {
                Ok(r) if r.interrupted => return Outcome::Stopped,
                Ok(r) => {
                    wrote.changed_albums += r.changed_albums;
                    wrote.changed_songs += r.changed_songs;
                    wrote.attached += r.attached;
                    wrote.merged += r.merged;
                    wrote.batches += r.batches;
                    wrote.longest_write = wrote.longest_write.max(r.longest_write);
                }
                Err(e) => return sync_error(e),
            }
        }
        catalog.extend(part);
        tell(Step::Reading { done: (n * CHUNK + chunk.len()).min(total), total });
        tell(Step::Written);
    }
    if complete {
        match job.client.scan_status() {
            Ok(now) if !now.scanning && now.last_scan == status.last_scan => {}
            Ok(_) => {
                log::info!("server sync: the server scanned while it was listed; nothing is removed this time");
                complete = false;
            }
            Err(e) => {
                log::warn!("server sync: the server's scans cannot be asked after the listing ({e}); nothing is removed this time");
                complete = false;
            }
        }
    }
    tell(Step::Finishing);
    let Some(_w) = wait_writer(cancel, tell) else { return Outcome::Stopped };
    let seen = crate::server::write::view_of(&catalog);
    let c = Catalog { albums: catalog, complete, server_scan: if complete { status.last_scan.clone() } else { None } };
    let mut report = match remote::sync(&mut conn, &job.identity, &c, &SyncOptions { confirm_removal: job.confirm_removal, ..opts }) {
        Ok(r) if r.interrupted => return Outcome::Stopped,
        Ok(r) => r,
        Err(e) => return sync_error(e),
    };
    report.changed_albums += wrote.changed_albums;
    report.changed_songs += wrote.changed_songs;
    report.attached += wrote.attached;
    report.merged += wrote.merged;
    report.batches += wrote.batches;
    report.longest_write = report.longest_write.max(wrote.longest_write);
    if let Err(e) = remote::reconcile(&mut conn) {
        return db_error(e);
    }
    // Albums gone: their covers may be unused now (still holding `WRITER`).
    if let Some(art) = ART.get() {
        crate::scanning::sweep_art(&conn, art, replaced || report.removed_albums > 0);
    }
    Outcome::Synced { report, failed, complete, seen }
}

/// The music of the server in the library, kept while the server signed in to now is another
/// (`Outcome::OtherServer`): what would leave the library if it is replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Elsewhere {
    /// Songs of the earlier server's catalog.
    pub songs: usize,
    /// Its plays not yet reported to it.
    pub plays: usize,
}

/// What the server signed in to is, to the one whose catalog the library holds.
#[derive(Debug, PartialEq, Eq)]
enum Kin {
    /// The same server (another address of it, or another spelling of the user, or another user).
    Same,
    /// Another: its catalog would replace the one kept.
    Other(Elsewhere),
    /// Nothing is kept to lose.
    Nothing,
}

/// Albums read, when the album ids do not settle it, to compare the songs' ids.
const SAMPLE: usize = 8;

/// The user of an identity ("address (user)").
fn user_of(identity: &str) -> &str {
    identity.split_once(" (").map_or("", |(_, u)| u.strip_suffix(')').unwrap_or(u))
}

/// Whether the server signed in to is the one whose catalog the library holds, by the ids it
/// gives its music: a server's ids are its own (Navidrome's are made from its library and the
/// files' paths), so another server shares few or none, and the same server under another
/// address all of them. Most of the kept albums listed: the same. Else (a server whose album
/// ids changed with an upgrade, while its songs' did not) a few albums spread over the listing
/// are read, and most of their songs known: the same. A sign-in refused, or the library not
/// read, ends the sync (the `Err`).
fn same_server(client: &Client, conn: &rusqlite::Connection, listed: &[subsonic::Album], cancel: &AtomicBool) -> Result<Kin, Box<Outcome>> {
    let db = |e: rusqlite::Error| Box::new(db_error(e));
    let (albums, songs) = remote::kept(conn).map_err(db)?;
    let plays = library::marks::queued_count(conn).map_err(db)? as usize;
    if albums == 0 && songs == 0 && plays == 0 {
        return Ok(Kin::Nothing);
    }
    let found = remote::kept_albums_among(conn, listed.iter().map(|a| a.id.as_str())).map_err(db)?;
    if albums > 0 && found * 2 >= albums {
        return Ok(Kin::Same);
    }
    let step = listed.len().div_ceil(SAMPLE).max(1);
    let sample: Vec<&str> = listed.iter().step_by(step).take(SAMPLE).map(|a| a.id.as_str()).collect();
    let mut read: Vec<String> = Vec::new();
    for r in read_albums(client, &sample, cancel) {
        match r {
            None => return Err(Box::new(Outcome::Stopped)),
            Some(Ok(a)) => read.extend(a.song.into_iter().map(|s| s.id)),
            Some(Err(e)) if is_sign_in(&e) => return Err(Box::new(Outcome::Server(e))),
            Some(Err(_)) => {}
        }
    }
    let known = remote::kept_songs_among(conn, read.iter().map(String::as_str)).map_err(db)?;
    if !read.is_empty() && known * 2 >= read.len() {
        return Ok(Kin::Same);
    }
    Ok(Kin::Other(Elsewhere { songs, plays }))
}

/// The stars the server has now, when the catalog is not listed again (favourites synced): every
/// song kept known starred or not. A sign-in refused is the sync's error; any other leaves the
/// view empty (the pass then only sends what changed here).
fn up_to_date_view(job: &Job, conn: &rusqlite::Connection) -> Result<View, Error> {
    if !job.marks {
        return Ok(View::new());
    }
    match job.client.starred2(None) {
        Ok(s) => {
            let kept: Vec<String> = conn.prepare("SELECT id FROM remote_songs").and_then(|mut st| st.query_map([], |r| r.get(0))?.collect()).unwrap_or_default();
            Ok(crate::server::write::view_of_starred(kept, &s.song))
        }
        Err(e) if is_sign_in(&e) => Err(e),
        Err(e) => {
            log::info!("server sync: the server's stars not read ({})", crate::server::class(&e));
            Ok(View::new())
        }
    }
}

/// The sign-in no longer works: the user must sign in again.
pub fn is_sign_in(e: &Error) -> bool {
    matches!(e, Error::WrongCredentials { .. })
}

// ---- the client's model onto the library's ----------------------------------------------

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn date(d: &Option<subsonic::ItemDate>) -> Option<String> {
    let d = d.as_ref()?;
    let y = d.year.filter(|y| *y > 0)?;
    Some(match (d.month.filter(|m| (1..=12).contains(m)), d.day.filter(|d| (1..=31).contains(d))) {
        (Some(m), Some(day)) => format!("{y:04}-{m:02}-{day:02}"),
        (Some(m), None) => format!("{y:04}-{m:02}"),
        _ => format!("{y:04}"),
    })
}

/// Unix milliseconds of an ISO 8601 UTC time as servers send them ("2024-05-01T10:00:00Z",
/// with or without fractions, "+00:00" or an offset).
pub fn iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (d, rest) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00Z"));
    let mut dp = d.split('-');
    let (y, mo, da): (i64, i64, i64) = (dp.next()?.parse().ok()?, dp.next()?.parse().ok()?, dp.next()?.parse().ok()?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || y > 9999 {
        return None;
    }
    let (time, offset_s) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => {
            let (t, z) = rest.split_at(i);
            let off = match z.chars().next() {
                Some('+' | '-') => {
                    let sign = if z.starts_with('-') { -1 } else { 1 };
                    let hm: Vec<i64> = z[1..].split(':').filter_map(|p| p.parse().ok()).collect();
                    if hm.iter().any(|v| !(0..10_000).contains(v)) {
                        return None;
                    }
                    match hm.as_slice() {
                        [h, m] => sign * (h * 3600 + m * 60),
                        [hm] if *hm >= 100 => sign * ((hm / 100) * 3600 + (hm % 100) * 60),
                        [h] => sign * h * 3600,
                        _ => 0,
                    }
                }
                _ => 0,
            };
            (t, off)
        }
        None => (rest, 0),
    };
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next().unwrap_or("0").parse().ok()?;
    let sec_s = tp.next().unwrap_or("0");
    let (whole, frac) = sec_s.split_once('.').unwrap_or((sec_s, ""));
    let sec: i64 = whole.parse().ok()?;
    if !(0..=24).contains(&h) || !(0..60).contains(&mi) || !(0..=60).contains(&sec) {
        return None;
    }
    // Its leading digits only (bytes, not characters: whatever else a server sends is no panic).
    let digits: String = frac.bytes().take_while(u8::is_ascii_digit).take(3).map(char::from).collect();
    let ms: i64 = format!("{digits:0<3}").parse().unwrap_or(0);
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset_s) * 1000) + ms)
}

/// A song's `starred` as the library keeps it: None when not starred (no time given), its time
/// (Unix ms) when starred, 0 when starred at a time that does not read. A time that does not
/// read is still a star: taken for none, it would unstar the favourite here.
pub fn star_time(starred: Option<&str>) -> Option<i64> {
    starred.map(|t| iso_ms(t).unwrap_or(0))
}

fn artists(list: &[subsonic::ArtistRef]) -> Vec<RemoteArtist> {
    list.iter().filter(|a| !a.name.trim().is_empty()).map(|a| RemoteArtist { name: a.name.clone(), mbid: None, sort_name: None }).collect()
}

fn names(list: &[subsonic::Named], single: &str) -> Vec<String> {
    let v: Vec<String> = list.iter().filter_map(|n| opt(&n.name)).collect();
    if v.is_empty() { opt(single).into_iter().collect() } else { v }
}

/// A server album with its songs, as the library takes it (`remote::RemoteAlbum`).
pub fn album_of(a: &subsonic::Album) -> RemoteAlbum {
    let artist = opt(&a.display_artist).unwrap_or_else(|| a.artist.clone());
    let songs = a.song.iter().filter(|s| !s.is_dir && (s.kind.is_empty() || s.kind == "music")).map(|s| song_of(s, &artist)).collect();
    RemoteAlbum {
        id: a.id.clone(),
        title: a.name.clone(),
        artist,
        artists: artists(&a.artists),
        artist_sort: None,
        date: a.year.filter(|y| *y > 0).map(|y| format!("{y:04}")),
        original_date: date(&a.original_release_date),
        release_date: date(&a.release_date),
        genres: names(&a.genres, &a.genre),
        label: a.record_labels.iter().find_map(|l| opt(&l.name)),
        release_types: a.release_types.iter().filter_map(|t| opt(t)).collect(),
        compilation: a.is_compilation,
        mb_album_id: opt(&a.music_brainz_id),
        created: a.created.as_deref().and_then(iso_ms),
        cover_art: a.cover_art.clone().filter(|c| !c.trim().is_empty()),
        starred: a.starred.as_deref().and_then(iso_ms),
        rating: None,
        play_count: a.play_count.map(|n| n as i64),
        songs,
    }
}

fn song_of(s: &subsonic::Song, album_artist: &str) -> RemoteSong {
    let own_album_artist = opt(&s.display_album_artist).filter(|aa| aa != album_artist);
    RemoteSong {
        id: s.id.clone(),
        title: s.title.clone(),
        artist: opt(&s.display_artist).unwrap_or_else(|| s.artist.clone()),
        artists: artists(&s.artists),
        album_artists: if own_album_artist.is_some() { artists(&s.album_artists) } else { Vec::new() },
        album_artist: own_album_artist,
        disc: s.disc_number.filter(|d| *d > 0),
        track: s.track.filter(|t| *t > 0),
        duration: f64::from(s.duration.unwrap_or(0)),
        date: s.year.filter(|y| *y > 0).map(|y| format!("{y:04}")),
        genres: names(&s.genres, &s.genre),
        mb_recording_id: opt(&s.music_brainz_id),
        artist_sort: None,
        created: s.created.as_deref().and_then(iso_ms),
        suffix: opt(&s.suffix),
        content_type: opt(&s.content_type),
        size: s.size.map_or(0, |n| n as i64),
        bitrate: s.bit_rate.filter(|b| *b > 0),
        bit_depth: s.bit_depth.and_then(|b| u8::try_from(b).ok()).filter(|b| *b > 0),
        sample_rate: s.sampling_rate.filter(|r| *r > 0),
        channels: s.channel_count.and_then(|c| u8::try_from(c).ok()).filter(|c| *c > 0),
        cover_art: s.cover_art.clone().filter(|c| !c.trim().is_empty()),
        starred: star_time(s.starred.as_deref()),
        rating: None,
        play_count: s.play_count.map(|n| n as i64),
        path: opt(&s.path),
    }
}

// ---- covers --------------------------------------------------------------------------------

/// What the covers thread is asked.
#[derive(Default)]
struct CoverWork {
    client: Option<Client>,
    /// Bumped with every client: a pass of an older one stops.
    generation: u64,
    /// Albums on screen without a cover, newest ask last.
    wanted: Vec<i64>,
    /// Look for every album that needs one.
    kicked: bool,
}

/// One thumbnail can fail on an otherwise healthy server. Remember these IDs
/// across visible-row requests; a sync or reconnection starts a fresh attempt.
#[derive(Default)]
struct CoverRetry {
    skipped: std::collections::HashSet<i64>,
    paused: bool,
}

struct Covers {
    work: Mutex<CoverWork>,
    ready: Condvar,
}

static COVERS: OnceLock<Covers> = OnceLock::new();
/// The art cache (from `covers_start`), swept after a sync that removed albums.
static ART: OnceLock<ArtCache> = OnceLock::new();

/// What `albums.art_checked` says of a server cover fetched (or found missing) in this cache
/// format.
pub fn cover_tag(cover: &str) -> String {
    format!("server:{}:{cover}", library::art::CACHE_VERSION)
}

/// The cover committed for an album. The server key ties a delayed notification to its artwork.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoverUpdate {
    pub album_id: i64,
    pub remote_art: String,
    pub stem: String,
}

/// Start the covers thread (once). The worker reports artwork after the database commit.
pub(super) fn covers_start(db: PathBuf, art: ArtCache, written: impl Fn(u64, Vec<CoverUpdate>) + Send + 'static) {
    if COVERS.set(Covers { work: Mutex::default(), ready: Condvar::new() }).is_err() {
        return;
    }
    let _ = ART.set(art.clone());
    let spawned = std::thread::Builder::new().name("mp-server-covers".into()).spawn(move || {
        library::background_thread();
        let c = COVERS.get().expect("set above");
        let mut conn: Option<rusqlite::Connection> = None;
        let mut retry = CoverRetry::default();
        loop {
            let (client, generation, wanted) = {
                let mut w = c.work.lock().unwrap_or_else(|e| e.into_inner());
                while w.client.is_none() || (!w.kicked && (w.wanted.is_empty() || retry.paused)) {
                    w = c.ready.wait(w).unwrap_or_else(|e| e.into_inner());
                }
                if std::mem::take(&mut w.kicked) {
                    retry = CoverRetry::default();
                }
                (w.client.clone().expect("checked"), w.generation, std::mem::take(&mut w.wanted))
            };
            if conn.is_none() {
                match library::db::open(&db) {
                    Ok(cn) => conn = Some(cn),
                    Err(e) => {
                        log::warn!("server covers: {e}");
                        continue;
                    }
                }
            }
            let conn = conn.as_mut().expect("opened");
            if let Err(e) = cover_pass(conn, &art, &client, generation, wanted, &mut retry, &written) {
                log::warn!("server covers: {e}");
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("server covers: cannot start ({e})");
    }
}

/// The client covers are fetched with; None stops fetching (signed out).
pub fn covers_client(client: Option<Client>) {
    let Some(c) = COVERS.get() else { return };
    let mut w = c.work.lock().unwrap_or_else(|e| e.into_inner());
    w.generation += 1;
    w.kicked = client.is_some();
    w.client = client;
    w.wanted.clear();
    c.ready.notify_all();
}

/// Whether a notification posted by the worker still belongs to the active connection.
pub(super) fn covers_current(generation: u64) -> bool {
    COVERS.get().is_some_and(|c| {
        let w = c.work.lock().unwrap_or_else(|e| e.into_inner());
        w.client.is_some() && w.generation == generation
    })
}

/// Look for covers to fetch (after a sync).
pub fn covers_kick() {
    let Some(c) = COVERS.get() else { return };
    c.work.lock().unwrap_or_else(|e| e.into_inner()).kicked = true;
    c.ready.notify_all();
}

/// An album on screen has no cover: its first.
pub fn covers_want(album: i64) {
    let Some(c) = COVERS.get() else { return };
    let mut w = c.work.lock().unwrap_or_else(|e| e.into_inner());
    w.wanted.retain(|a| *a != album);
    w.wanted.push(album);
    c.ready.notify_all();
}

/// The cover id of album `id` when it needs fetching (an album only the server has, whose cover
/// was not fetched in this cache format).
fn needs(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row("SELECT remote_art, art_checked FROM albums WHERE id = ?1 AND folder LIKE 'navidrome:%'", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(row.and_then(|(cover, checked)| cover.filter(|c| checked.as_deref() != Some(cover_tag(c).as_str()))))
}

/// The next albums that need a cover, newest first (the albums asked for on screen before them).
fn next_batch(conn: &rusqlite::Connection, wanted: &mut Vec<i64>, skip: &std::collections::HashSet<i64>) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut out = Vec::new();
    while out.len() < COVER_READERS
        && let Some(id) = wanted.pop()
    {
        if !skip.contains(&id) && !out.iter().any(|(album, _)| *album == id)
            && let Some(c) = needs(conn, id)?
        {
            out.push((id, c));
        }
    }
    if out.len() < COVER_READERS {
        let mut st = conn.prepare_cached(
            "SELECT id, remote_art FROM albums WHERE remote_art IS NOT NULL AND folder LIKE 'navidrome:%'
               AND art_checked IS NOT ('server:' || ?1 || ':' || remote_art)
             ORDER BY added_at DESC, id DESC LIMIT ?2",
        )?;
        let limit = (COVER_READERS * 2 + skip.len()) as i64;
        for row in st.query_map(rusqlite::params![library::art::CACHE_VERSION, limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, c) = row?;
            if out.len() < COVER_READERS && !skip.contains(&id) && !out.iter().any(|(o, _)| *o == id) {
                out.push((id, c));
            }
        }
    }
    Ok(out)
}

/// What fetching one cover came to.
enum Fetched {
    Stored(String),
    /// The server has none (or not an image): not asked again in this format.
    Missing,
    /// The server could not serve this image. Other covers can still succeed.
    Deferred,
    /// Connectivity, authentication or another service-wide failure.
    Pause(Error),
}

fn fetch(client: &Client, art: &ArtCache, cover: &str) -> Fetched {
    match client.cover_art(cover, Some(library::art::FULL_PX)) {
        Ok(c) => match art.store(&c.bytes) {
            Ok(stem) => Fetched::Stored(stem),
            Err(e) => {
                log::info!("server cover is not an image the app reads ({e})");
                Fetched::Missing
            }
        },
        Err(Error::NotFound(_) | Error::BadResponse(_)) => Fetched::Missing,
        // Subsonic can also report an item error in a successful HTTP response.
        Err(Error::Server { http_status: Some(200 | 500) | None, .. }) => Fetched::Deferred,
        Err(e) => Fetched::Pause(e),
    }
}

fn cover_pass(conn: &mut rusqlite::Connection, art: &ArtCache, client: &Client, generation: u64, mut wanted: Vec<i64>, retry: &mut CoverRetry, written: &dyn Fn(u64, Vec<CoverUpdate>)) -> rusqlite::Result<()> {
    let c = COVERS.get().expect("started");
    let mut failed_batches = 0;
    let mut fetched = 0;
    loop {
        {
            let mut w = c.work.lock().unwrap_or_else(|e| e.into_inner());
            if w.generation != generation || w.client.is_none() {
                return Ok(());
            }
            wanted.append(&mut w.wanted);
        }
        let batch = next_batch(conn, &mut wanted, &retry.skipped)?;
        if batch.is_empty() {
            if fetched > 0 {
                log::info!("server covers: {fetched} fetched, {} deferred", retry.skipped.len());
            }
            return Ok(());
        }
        let results: Vec<Fetched> = std::thread::scope(|s| {
            let handles: Vec<_> = batch.iter().map(|(_, cover)| s.spawn(move || fetch(client, art, cover))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or(Fetched::Pause(Error::BadResponse("panicked".into())))).collect()
        });
        let mut pause = None;
        let all_deferred = results.iter().all(|r| matches!(r, Fetched::Deferred));
        library::db::let_waiting_in();
        let tx = library::db::write(conn)?;
        // The account can change while a slow cover request is in flight. Check again
        // inside the transaction, before recording anything against the current catalog.
        if c.work.lock().unwrap_or_else(|e| e.into_inner()).generation != generation {
            return Ok(());
        }
        let mut done = Vec::new();
        for ((id, cover), r) in batch.into_iter().zip(results) {
            match r {
                Fetched::Stored(stem) => {
                    let changed = tx.execute("UPDATE albums SET art = ?2, art_checked = ?3 WHERE id = ?1 AND remote_art IS ?4", rusqlite::params![id, stem, cover_tag(&cover), cover])?;
                    if changed > 0 {
                        done.push(CoverUpdate { album_id: id, remote_art: cover, stem });
                        fetched += 1;
                    }
                }
                Fetched::Missing => {
                    tx.execute("UPDATE albums SET art_checked = ?2 WHERE id = ?1 AND remote_art IS ?3", rusqlite::params![id, cover_tag(&cover), cover])?;
                }
                Fetched::Deferred => {
                    retry.skipped.insert(id);
                }
                Fetched::Pause(e) => pause = Some(e),
            }
        }
        tx.commit()?;
        if !done.is_empty() {
            written(generation, done);
        }
        if let Some(e) = pause {
            log::info!("server covers: paused until the next sync ({e})");
            retry.paused = true;
            return Ok(());
        }
        // Two consecutive batches with no working cover suggest the whole service is failing.
        failed_batches = if all_deferred { failed_batches + 1 } else { 0 };
        if failed_batches >= 2 {
            log::info!("server covers: consecutive batches failed, paused until the next sync");
            retry.paused = true;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_read() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2024-05-01T10:00:00Z"), Some(1_714_557_600_000));
        assert_eq!(iso_ms("2024-05-01T10:00:00.123456789Z"), Some(1_714_557_600_123));
        assert_eq!(iso_ms("2024-05-01T12:00:00+02:00"), Some(1_714_557_600_000));
        assert_eq!(iso_ms("2024-05-01"), Some(1_714_521_600_000));
        assert_eq!(iso_ms("2000-02-29T00:00:00Z"), Some(951_782_400_000));
        assert_eq!(iso_ms("garbage"), None);
        assert_eq!(iso_ms("2024-13-01T00:00:00Z"), None);
        // A misbehaving server's: no time, never a panic.
        assert_eq!(iso_ms("2024-05-01T10:00:00.1éZ"), Some(1_714_557_600_100));
        assert_eq!(iso_ms("2024-05-01T10:00:00.é1Z"), Some(1_714_557_600_000));
        assert_eq!(iso_ms("2024-05-01T99999999999999999:00:00Z"), None);
        assert_eq!(iso_ms("2024-05-01T10:61:00Z"), None);
        assert_eq!(iso_ms("9223372036854775807-05-01T10:00:00Z"), None);
        assert_eq!(iso_ms("2024-05-01T10:00:00+9999999999999999:00"), None);
    }

    #[test]
    fn an_album_maps_onto_the_library() {
        let mut a = subsonic::mock::album("al1", "Ann", "Night", 1999, "2024-05-01T10:00:00Z", &[("One", 1, 200, 12345, "Ann/Night/01 One.flac"), ("Two", 2, 180, 0, "")]);
        a.original_release_date = Some(subsonic::ItemDate { year: Some(1997), month: Some(3), day: Some(21) });
        a.release_date = Some(subsonic::ItemDate::default());
        a.music_brainz_id = " MB-1 ".into();
        a.record_labels = vec![subsonic::Named { name: "Label".into() }];
        a.song[1].display_album_artist = "Bob".into();
        a.song.push(subsonic::Song { id: "v".into(), kind: "video".into(), ..Default::default() });
        let r = album_of(&a);
        assert_eq!((r.title.as_str(), r.artist.as_str(), r.date.as_deref(), r.original_date.as_deref(), r.release_date.as_deref()), ("Night", "Ann", Some("1999"), Some("1997-03-21"), None));
        assert_eq!((r.mb_album_id.as_deref(), r.label.as_deref(), r.created, r.cover_art.as_deref()), (Some("MB-1"), Some("Label"), Some(1_714_557_600_000), Some("al-al1")));
        assert_eq!(r.songs.len(), 2, "music only");
        let (s0, s1) = (&r.songs[0], &r.songs[1]);
        assert_eq!((s0.path.as_deref(), s0.size, s0.duration, s0.track, s0.album_artist.as_deref()), (Some("Ann/Night/01 One.flac"), 12345, 200.0, Some(1), None));
        assert_eq!((s1.path.as_deref(), s1.size, s1.album_artist.as_deref()), (None, 0, Some("Bob")), "no path, no size; its own album artist");
        assert_eq!(s0.genres, ["Rock"]);
    }

    use subsonic::mock::{self, Mock, Trouble};

    /// A cover as a server sends one: a small PNG of one colour.
    fn png(rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(48, 48, image::Rgb(rgb));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn library_of(n: usize) -> mock::Library {
        let mut lib = mock::Library { last_scan: Some("2026-11-01T00:00:00Z".into()), ..Default::default() };
        lib.albums = (0..n)
            .map(|i| {
                mock::album(
                    &format!("al{i:04}"),
                    &format!("Artist {}", i % 7),
                    &format!("Record {i:04}"),
                    1990 + (i % 30) as i32,
                    "2026-01-01T00:00:00Z",
                    &[("One", 1, 200, 1000 + i as u64, ""), ("Two", 2, 180, 2000 + i as u64, "")],
                )
            })
            .collect();
        lib
    }

    struct Fixture {
        dir: PathBuf,
        mock: Mock,
    }

    impl Fixture {
        fn new(name: &str, lib: mock::Library) -> Self {
            let dir = std::env::temp_dir().join(format!("mp-serversync-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Fixture { dir, mock: Mock::start("ann", "pw", lib).unwrap() }
        }
        fn job(&self, force: bool) -> Job {
            let (creds, _) = subsonic::sign_in(&self.mock.url, "ann", "pw").unwrap();
            Job { client: Client::new(creds, true).unwrap(), identity: "mock (ann)".into(), db: self.dir.join("library.db"), force, confirm_removal: false, cancel: Arc::default(), marks: false }
        }
        fn run(&self, job: &Job) -> (Outcome, Vec<Step>) {
            let steps = Mutex::new(Vec::new());
            let o = run(job, &|s| steps.lock().unwrap().push(s));
            (o, steps.into_inner().unwrap())
        }
        fn conn(&self) -> rusqlite::Connection {
            library::db::open(&self.dir.join("library.db")).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A whole catalog of more than one page, read three albums at a time, written as it is
    /// read; a second sync with nothing scanned on the server asks one question and lists
    /// nothing; after a scan there it lists again, and what went is removed.
    #[test]
    fn a_catalog_is_listed_in_pages_and_only_again_after_the_server_scans() {
        let f = Fixture::new("pages", library_of(530));
        let (o, steps) = f.run(&f.job(false));
        let Outcome::Synced { report, failed, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((report.albums, report.songs, failed, complete), (530, 1060, 0, true));
        assert_eq!(f.mock.count("getAlbumList2"), 2, "two pages of up to 500");
        assert_eq!(f.mock.count("getAlbum"), 530);
        assert!(steps.iter().filter(|s| **s == Step::Written).count() >= 9, "written as read, a chunk at a time");
        assert!(steps.contains(&Step::Reading { done: 530, total: 530 }));
        let conn = f.conn();
        assert_eq!(remote::counts(&conn).unwrap().server, 1060);
        let s = remote::server(&conn).unwrap().unwrap();
        assert!(s.synced_at.is_some() && s.sync_started.is_none());
        assert_eq!(s.server_scan.as_deref(), Some("2026-11-01T00:00:00Z"));
        // Nothing scanned on the server since: one question.
        let job = f.job(false);
        f.mock.clear_requests();
        assert!(matches!(f.run(&job).0, Outcome::UpToDate(_)));
        assert_eq!(f.mock.requests(), ["getScanStatus"]);
        // Asked for (Sync now): listed anyway, nothing written again.
        let (o, _) = f.run(&f.job(true));
        let Outcome::Synced { report, .. } = o else { panic!("{o:?}") };
        assert_eq!((report.changed_albums, report.changed_songs), (0, 0));
        // The server scanned: an album gone, one changed.
        f.mock.edit(|l| {
            l.albums.retain(|a| a.id != "al0007");
            l.albums[0].song[0].title = "Uno".into();
            l.last_scan = Some("2026-11-02T00:00:00Z".into());
        });
        let (o, _) = f.run(&f.job(false));
        let Outcome::Synced { report, complete, .. } = o else { panic!("{o:?}") };
        assert!(complete);
        assert_eq!((report.removed_albums, report.removed_songs, report.changed_songs), (1, 2, 1));
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 1058);
    }

    /// An album that cannot be read leaves the listing incomplete: nothing is removed, and the
    /// next sync lists again whatever the server says of its scans. A server gone, or one that no
    /// longer takes the sign-in, is said as such; the library stays as it was.
    #[test]
    fn errors_keep_the_library_as_it_was() {
        let f = Fixture::new("errors", library_of(12));
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }));
        f.mock.edit(|l| l.albums.truncate(6));
        f.mock.set_trouble(Trouble::FailAlbum("al0002".into()));
        let (o, _) = f.run(&f.job(true));
        let Outcome::Synced { report, failed, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((failed, complete, report.removed_songs), (1, false, 0), "incomplete: nothing removed");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 24);
        let s = remote::server(&f.conn()).unwrap().unwrap();
        assert_eq!(s.server_scan, None, "an incomplete sync is not one to skip the next after");
        f.mock.set_trouble(Trouble::None);
        let job = f.job(false);
        f.mock.clear_requests();
        let (o, _) = f.run(&job);
        assert!(matches!(o, Outcome::Synced { complete: true, .. }), "{o:?}");
        assert!(f.mock.count("getAlbumList2") > 0, "listed again although the server did not scan");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 12);
        let job = f.job(true);
        f.mock.set_trouble(Trouble::Down);
        assert!(matches!(f.run(&job).0, Outcome::Server(Error::Network(_))));
        f.mock.set_trouble(Trouble::Unavailable);
        assert!(matches!(f.run(&job).0, Outcome::Server(Error::Server { http_status: Some(503), .. })));
        f.mock.set_trouble(Trouble::WrongPassword);
        let o = f.run(&job).0;
        assert!(matches!(&o, Outcome::Server(e) if is_sign_in(e)), "{o:?}");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 12, "the library as last synced");
    }

    /// An album that comes without the songs the server counts for it is an answer cut short:
    /// read as failed, so the listing is not complete and none of its songs is taken for gone.
    /// One the server counts empty is emptied, and its songs go.
    #[test]
    fn an_album_that_comes_without_its_songs_is_not_read() {
        let f = Fixture::new("empty-album", library_of(12));
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }));
        f.mock.edit(|l| l.albums[3].song.clear());
        let (o, _) = f.run(&f.job(true));
        let Outcome::Synced { report, failed, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((failed, complete, report.removed_songs), (1, false, 0));
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 24);
        f.mock.edit(|l| l.albums[3].song_count = 0);
        let (o, _) = f.run(&f.job(true));
        let Outcome::Synced { report, failed, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((failed, complete, report.removed_songs), (0, true, 2));
    }

    /// Stop between albums: what was written stays, the next sync goes on and ends whole. Another
    /// server's catalog in the library is removed before this one's is written.
    #[test]
    fn a_stopped_sync_keeps_what_it_wrote_and_another_server_replaces_the_first() {
        let f = Fixture::new("stop", library_of(200));
        f.mock.set_delay_ms(2);
        let job = f.job(true);
        let cancel = job.cancel.clone();
        let written_steps = Mutex::new(0usize);
        let o = run(&job, &|s| {
            if s == Step::Written {
                let mut n = written_steps.lock().unwrap();
                *n += 1;
                if *n == 2 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert!(matches!(o, Outcome::Stopped), "{o:?}");
        let written = remote::counts(&f.conn()).unwrap().server;
        assert!((1..400).contains(&written), "{written} tracks written before the stop");
        assert_eq!(remote::server(&f.conn()).unwrap().unwrap().server_scan, None, "marked as no sync to skip the next after");
        f.mock.set_delay_ms(0);
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }), "an unfinished sync is not skipped");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 400);
        // Signed in elsewhere: the first server's music stays until the replacement is
        // confirmed, then goes.
        let other = Fixture::new("stop-other", other_library(3));
        let mut job = other.job(true);
        job.db = f.dir.join("library.db");
        job.identity = "other (bob)".into();
        let (o, steps) = f.run(&job);
        assert!(matches!(o, Outcome::OtherServer(Elsewhere { songs: 400, plays: 0 })), "{o:?}");
        assert!(!steps.contains(&Step::Replacing));
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 400, "kept until confirmed");
        assert_eq!(remote::server(&f.conn()).unwrap().unwrap().identity, "mock (ann)");
        job.confirm_removal = true;
        let (o, steps) = f.run(&job);
        assert!(matches!(o, Outcome::Synced { .. }), "{o:?}");
        assert!(steps.contains(&Step::Replacing));
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 6);
        assert_eq!(remote::server(&f.conn()).unwrap().unwrap().identity, "other (bob)");
    }

    /// Another server's albums and songs: the same music under ids of its own.
    fn other_library(n: usize) -> mock::Library {
        let mut lib = library_of(n);
        for a in &mut lib.albums {
            a.id = format!("x-{}", a.id);
            for s in &mut a.song {
                s.id = format!("x-{}", s.id);
            }
        }
        lib
    }

    /// The same server signed in to under another address (after a lost sign-in): nothing of
    /// its music goes, its plays not yet reported and favourites agreed stay; under another
    /// user, the agreements go (that user's stars are not the ones agreed) and the plays stay.
    /// A server whose album ids changed (an upgrade) is known by its songs'.
    #[test]
    fn the_same_server_under_another_name_keeps_its_music() {
        let f = Fixture::new("renamed", library_of(40));
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }));
        let c = f.conn();
        let track: i64 = c.query_row("SELECT id FROM tracks WHERE remote_id IS NOT NULL ORDER BY id LIMIT 1", [], |r| r.get(0)).unwrap();
        library::query::toggle_favourite_track(&c, track).unwrap();
        library::marks::agree_star(&c, "al0000-1", true, None).unwrap();
        library::query::record_plays_for(&c, &mut vec![(track, 1_700_000_000)], true).unwrap();
        let plays_before: i64 = c.query_row("SELECT COUNT(*) FROM plays", [], |r| r.get(0)).unwrap();
        assert!(plays_before > 0 && library::marks::queued_count(&c).unwrap() == 1);
        drop(c);
        let mut job = f.job(true);
        job.identity = "http://localhost:4533 (Ann)".into();
        let (o, steps) = f.run(&job);
        assert!(matches!(o, Outcome::Synced { complete: true, ref report, .. } if report.removed_songs == 0), "{o:?}");
        assert!(!steps.contains(&Step::Replacing));
        let c = f.conn();
        assert_eq!(remote::server(&c).unwrap().unwrap().identity, "http://localhost:4533 (Ann)");
        assert_eq!(remote::counts(&c).unwrap().server, 80);
        assert!(library::query::is_favourite_track(&c, track).unwrap());
        assert_eq!(library::marks::queued_count(&c).unwrap(), 1, "the play not yet reported waits");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM remote_marks WHERE starred = 1", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "the same user: agreed as before");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM plays", [], |r| r.get::<_, i64>(0)).unwrap(), plays_before);
        drop(c);
        // Another user of it: the catalog and the plays stay, the agreements go.
        job.identity = "http://localhost:4533 (bob)".into();
        assert!(matches!(f.run(&job).0, Outcome::Synced { complete: true, .. }));
        let c = f.conn();
        assert_eq!(remote::counts(&c).unwrap().server, 80);
        assert!(library::query::is_favourite_track(&c, track).unwrap());
        assert_eq!(library::marks::queued_count(&c).unwrap(), 1);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM remote_marks", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(c);
        // Its album ids changed, its songs' did not: the same server still.
        f.mock.edit(|l| {
            for a in &mut l.albums {
                a.id = format!("new-{}", a.id);
            }
        });
        job.identity = "https://music.example.org (bob)".into();
        let (o, _) = f.run(&job);
        assert!(matches!(o, Outcome::Synced { .. }), "{o:?}");
        assert!(f.mock.count("getAlbum") > 0);
        assert_eq!(remote::server(&f.conn()).unwrap().unwrap().identity, "https://music.example.org (bob)");
        assert_eq!(library::marks::queued_count(&f.conn()).unwrap(), 1);
    }

    /// The server scanning while it is listed (its list may shift under the pages, and an
    /// album then go unlisted): nothing is removed, and the next sync lists again.
    #[test]
    fn a_listing_the_server_changed_under_removes_nothing() {
        let f = Fixture::new("shifted", library_of(530));
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }));
        // An album before the second page goes as that page is asked for: the first album of
        // that page moves back onto the first and is never listed.
        f.mock.set_delay_ms(30);
        let job = f.job(true);
        f.mock.clear_requests();
        let o = std::thread::scope(|s| {
            s.spawn(|| {
                let t0 = std::time::Instant::now();
                while f.mock.count("getAlbumList2") < 2 && t0.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                f.mock.edit(|l| {
                    l.albums.retain(|a| a.id != "al0003");
                    l.last_scan = Some("2026-11-02T00:00:00Z".into());
                });
                f.mock.set_delay_ms(0);
            });
            f.run(&job).0
        });
        let Outcome::Synced { report, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((complete, report.removed_albums), (false, 0), "an album listed under a changing list is not taken for gone");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 1060);
        // Scanning when asked: nothing removed either.
        f.mock.edit(|l| l.scanning = true);
        let Outcome::Synced { report, complete, .. } = f.run(&f.job(true)).0 else { panic!() };
        assert_eq!((complete, report.removed_albums), (false, 0));
        // The scan done: the album that went is removed.
        f.mock.edit(|l| l.scanning = false);
        let Outcome::Synced { report, complete, .. } = f.run(&f.job(false)).0 else { panic!() };
        assert_eq!((complete, report.removed_albums), (true, 1));
    }

    /// A server that ignores `offset` (each page its first): the listing ends at the first page
    /// that brings nothing new, each album once, and is not complete, so nothing is removed.
    #[test]
    fn a_server_that_ignores_the_offset_is_not_listed_forever() {
        let f = Fixture::new("offset", library_of(530));
        assert!(matches!(f.run(&f.job(false)).0, Outcome::Synced { complete: true, .. }));
        f.mock.set_trouble(Trouble::IgnoreOffset);
        f.mock.clear_requests();
        let job = f.job(true);
        let o = std::thread::scope(|s| {
            let h = s.spawn(|| f.run(&job).0);
            let t0 = std::time::Instant::now();
            while !h.is_finished() && t0.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(20));
            }
            job.cancel.store(true, Ordering::Relaxed);
            h.join().unwrap()
        });
        let Outcome::Synced { report, complete, .. } = o else { panic!("{o:?}") };
        assert_eq!((report.albums, complete, report.removed_albums), (500, false, 0));
        assert_eq!(f.mock.count("getAlbumList2"), 2);
        assert_eq!(f.mock.count("getAlbum"), 500, "each album read once");
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 1060);
    }

    /// A sync waits for a scan's writes (and says so), and goes on once the scan is done.
    #[test]
    fn a_sync_waits_for_a_scan_to_write() {
        let f = Fixture::new("writer", library_of(4));
        let job = f.job(true);
        let guard = writer();
        let saw_wait = AtomicBool::new(false);
        std::thread::scope(|s| {
            let h = s.spawn(|| {
                run(&job, &|st| {
                    if st == Step::Waiting {
                        saw_wait.store(true, Ordering::Relaxed);
                    }
                })
            });
            let t0 = std::time::Instant::now();
            while !saw_wait.load(Ordering::Relaxed) && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(saw_wait.load(Ordering::Relaxed), "waiting said");
            assert_eq!(remote::counts(&f.conn()).unwrap().server, 0, "nothing written meanwhile");
            drop(guard);
            assert!(matches!(h.join().unwrap(), Outcome::Synced { .. }));
        });
        assert_eq!(remote::counts(&f.conn()).unwrap().server, 8);
    }

    /// Covers of albums only the server has come into the art cache, newest album first; a
    /// cover the server lacks is not asked for again; a later cache format fetches them again.
    #[test]
    fn server_covers_come_into_the_art_cache_newest_first() {
        let mut lib = library_of(6);
        for (i, a) in lib.albums.iter_mut().enumerate() {
            a.created = Some(format!("2026-01-{:02}T00:00:00Z", i + 1));
            if i != 2 {
                lib.covers.insert(format!("al-{}", a.id), png([40 * i as u8, 200, 90]));
            }
        }
        let f = Fixture::new("covers", lib);
        assert!(matches!(f.run(&f.job(true)).0, Outcome::Synced { .. }));
        let art = ArtCache::new(f.dir.join("art"));
        let got: Arc<Mutex<Vec<CoverUpdate>>> = Arc::default();
        let g = got.clone();
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let delivered = generation.clone();
        covers_start(f.dir.join("library.db"), art.clone(), move |ticket, updates| {
            delivered.store(ticket, Ordering::Relaxed);
            g.lock().unwrap().extend(updates);
        });
        let client = f.job(false).client;
        f.mock.set_trouble(Trouble::FailCover("al-al0005".into()));
        covers_client(Some(client.clone()));
        let wait = |n: usize| {
            let t0 = std::time::Instant::now();
            while got.lock().unwrap().len() < n && t0.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(got.lock().unwrap().len(), n, "committed cover notifications");
        };
        let wait_requests = |n| {
            let start = std::time::Instant::now();
            while f.mock.count("getCoverArt") < n && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(f.mock.count("getCoverArt"), n);
        };
        // A 500 in the first batch leaves only that cover pending. The missing cover still
        // gets checked, and the healthy covers from the second batch still reach the UI.
        wait(4);
        let conn = f.conn();
        let (id, checked): (i64, Option<String>) = conn.query_row("SELECT id, art_checked FROM albums WHERE remote_id = 'al0005'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(checked.is_none());
        assert_eq!(f.mock.count("getCoverArt"), 6);
        let order: Vec<String> = f.mock.requests().into_iter().filter(|r| r.starts_with("getCoverArt")).collect();
        assert!(order[..3].iter().any(|r| r.ends_with("al-al0005")), "newest first: {order:?}");
        assert!(order[3..].iter().any(|r| r.ends_with("al-al0000")), "oldest last: {order:?}");
        f.mock.set_trouble(Trouble::None);
        covers_want(id);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(f.mock.count("getCoverArt"), 6, "scrolling does not retry a deferred cover");
        covers_kick();
        wait(5);
        let arts: Vec<(i64, String, Option<String>, Option<String>)> = conn
            .prepare("SELECT id, remote_art, art, art_checked FROM albums ORDER BY added_at DESC")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(arts.iter().filter(|a| a.2.is_some()).count(), 5, "{arts:?}");
        for (id, cover, stem, checked) in &arts {
            assert_eq!(checked.as_deref(), Some(cover_tag(cover).as_str()), "recorded: {cover}");
            if let Some(stem) = stem {
                assert!(art.is_current(stem), "the cache's own sizes, backdrop and accent");
                assert!(got.lock().unwrap().contains(&CoverUpdate { album_id: *id, remote_art: cover.clone(), stem: stem.clone() }));
            }
        }
        assert_eq!(f.mock.count("getCoverArt"), 7, "only the failed cover was retried");
        // Kicked again: nothing more is needed.
        covers_kick();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(f.mock.count("getCoverArt"), 7);
        // A cache format of before: every server cover is fetched again.
        conn.execute("UPDATE albums SET art_checked = 'server:1:' || remote_art WHERE art IS NOT NULL", []).unwrap();
        got.lock().unwrap().clear();
        covers_kick();
        wait(5);
        assert_eq!(f.mock.count("getCoverArt"), 12);
        // The UI changes one album's cover while a batch is fetching. Hold the write lock until
        // both requests arrive so the worker sees the old key, then the new value at its UPDATE.
        let previous = generation.load(Ordering::Relaxed);
        assert!(covers_current(previous));
        covers_client(None);
        assert!(!covers_current(previous));
        conn.execute("UPDATE albums SET art_checked = NULL WHERE remote_id IN ('al0003', 'al0004')", []).unwrap();
        let tx = library::db::write(&conn).unwrap();
        tx.execute("UPDATE albums SET remote_art = NULL, art = NULL WHERE remote_id = 'al0004'", []).unwrap();
        got.lock().unwrap().clear();
        covers_client(Some(client.clone()));
        wait_requests(14);
        tx.commit().unwrap();
        wait(1);
        assert_eq!(got.lock().unwrap()[0].remote_art, "al-al0003");
        assert!(!covers_current(previous));
        assert!(covers_current(generation.load(Ordering::Relaxed)));
        covers_client(None);
        // A service-wide 503 stops the pass after one batch. Visible-row wakeups leave it
        // paused; an ordinary sync after recovery resumes the queue.
        conn.execute("UPDATE albums SET art_checked = NULL WHERE remote_art IS NOT NULL", []).unwrap();
        f.mock.clear_requests();
        got.lock().unwrap().clear();
        f.mock.set_trouble(Trouble::Unavailable);
        covers_client(Some(client));
        wait_requests(COVER_READERS);
        covers_want(id);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(f.mock.count("getCoverArt"), COVER_READERS);
        f.mock.set_trouble(Trouble::None);
        covers_kick();
        wait(4);
        covers_client(None);
    }
}
