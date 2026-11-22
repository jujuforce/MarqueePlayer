//! A Navidrome (Subsonic / OpenSubsonic) server as a library source beside the music folders:
//! one library, nothing listed twice. This module owns the database side only; the app fetches
//! the catalog (no network code here) and hands it over as plain structs (`Catalog`).
//!
//! **One row per track and per album, whatever its sources.** A track the server has and a local
//! file also has is one row: the file's, as the scanner wrote it (local tags win; the server never
//! overwrites what a file says), with `tracks.remote_id` set. A track only the server has is a row
//! of its own whose path is no file's (`song_path`: `navidrome:song/<id>`; `is_remote` tells it
//! apart). An album likewise: a local album the server also has carries `albums.remote_id`; one
//! only the server has is a row of its own whose folder is no folder (`navidrome:album/<id>`) and
//! whose key is `mb:<release id>` (the key a local copy of it would have, so they are one album
//! by construction) or `nd:<album id>`. So every query, shelf, search, smart playlist, artist and
//! genre page reads server tracks as it reads local ones; server albums are filled through the
//! scanner's own code (`relations`, `scan::album_values`, `tags::map_dates`, `track_tags`), so they
//! sort, group and search exactly as local albums do.
//!
//! **The catalog is kept** (`remote_albums`, `remote_songs`: each entry as the server listed it,
//! and its hash), so what the library shows of the server is worked out from it (`place`) with no
//! request to the server: after a sync, after a scan (`reconcile`), when a file goes away (its row
//! becomes the server's again: `to_server_only`), when one comes back.
//!
//! **The same file** is the strongest sign (`file_key`): a server usually serves the very files the
//! music folders hold (Navidrome on the same disk, or a copy of it). A server song whose size in
//! bytes equals a local file's and whose path ends in the same folder and file name (the last two
//! components; separators, case, spacing and Unicode form ignored: the server's path is relative
//! to its music folder, the local one absolute) is that file. It decides before any tag does, but
//! never against MusicBrainz ids that both have and that differ.
//!
//! **Matching a server album to a local one** (`find_local_album`):
//! 1. By MusicBrainz release id: a local album keyed `mb:<id>` is the same album.
//! 2. Else by its files (`by_files`): when the server album's songs that are local files are all
//!    files of one local album (enough of them, by the half rule below, and no number where the
//!    two disagree), it is that album, even when the titles are spelled a little differently (the
//!    tags on each side differ; the files do not). Release ids that differ, a file two local
//!    albums hold alike, or files spread over two local albums: not this way.
//! 3. Else by album artist and title, by `names::norm` as the scanner groups (case, spacing and
//!    Unicode form ignored, accents not; "Various Artists" and its spellings are one), and only
//!    when it is plainly the same release: not when both have release ids and they differ, not
//!    when both have a year and it differs (a reissue, a remaster, a deluxe edition of another
//!    year), only when their tracks agree (no disc and track number where both have a track and
//!    the two disagree on title and duration, and at least half of the smaller side's tracks
//!    matched: a local copy of part of an album is that album), and only when exactly one local
//!    album passes and no other server album holds it. When in doubt, two rows: a wrong merge
//!    would hand one album's tracks and history to another, two rows only show it twice.
//!
//! **Matching tracks inside a matched album** (`match_tracks`): by the same file first (unless
//! their recording ids differ); by MusicBrainz recording id when both have one (different ids never
//! match); else by disc and track number with the folded
//! title (`fold::fold`) or the duration (within 2 s) agreeing; else, for tracks without numbers,
//! by folded title and duration together. Every match is the only candidate both ways.
//!
//! **What is never done**: the server's starred and play counts are kept with the catalog and
//! never applied to the user's favourites or plays here (favourites are kept in step with the
//! server by `marks`, which the app runs; plays never); its ratings are not read at all;
//! a server never changes a local track's tags or path, or a local album's dates.
//!
//! **Removals** (`sync`): only when the caller says the listing was the whole catalog
//! (`Catalog::complete`), and not when it is implausibly smaller, by the scanner's own guard
//! (`scan::GUARD_MIN_MISSING`, `scan::GUARD_FOUND_RATIO`: at least 25 songs gone and fewer than
//! one in ten left) unless confirmed (`SyncOptions::confirm_removal`). A server-only track that
//! goes takes its plays with it, as a deleted file's does; a track with a file only loses its
//! `remote_id`.
//!
//! **Sharing the database**: a sync writes a batch of albums at a time (about `BATCH_SONGS`
//! songs), each in a transaction of its own, letting the app's own writes in between
//! (`db::let_waiting_in`), as a scan does; artists' and genres' names are settled in small
//! transactions every few seconds. An interrupted sync keeps what it wrote and the next one goes
//! on from there: an entry whose hash has not changed and is placed is not written again, so a
//! second sync of the same catalog changes nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::audio::Codec;
use crate::db::{now, now_ms};
use crate::names::{self, Credit};
use crate::relations::{self, Touched};
use crate::scan;
use crate::tags;

/// What every path and folder that is no file's starts with.
pub const SCHEME: &str = "navidrome:";
const SONG_PREFIX: &str = "navidrome:song/";
const ALBUM_PREFIX: &str = "navidrome:album/";
/// SQL, for a track aliased `t`: its path is a file's.
pub(crate) const LOCAL_T: &str = "t.path NOT LIKE 'navidrome:%'";

/// Songs written per transaction by `sync` (whole albums: a batch ends where an album does).
const BATCH_SONGS: usize = 512;
/// How often the names of the artists and genres a sync touched are worked out again, and how
/// many in one transaction (as a scan does).
const SETTLE_EVERY: Duration = Duration::from_secs(3);
const SETTLE_CHUNK: usize = 8;
/// Albums `reconcile` and `disconnect` handle per transaction.
const ALBUMS_PER_TX: usize = 64;

/// A path or album folder that is no file's: the server's track or album (`song_path`,
/// `album_folder`). Where a file would be shown, opened, edited or read, there is none.
pub fn is_remote(path: &str) -> bool {
    path.starts_with(SCHEME)
}

/// What reading or editing a server's own track says (it has no file here).
pub const NO_FILE: &str = "This track is on the server only: there is no file here to read or edit";

/// `Err(NO_FILE)` for a server's own track: the entry points that read or write a file
/// (`tags`, `meta`, `edit`) refuse it at once, with words the app can show.
pub fn refuse(path: &std::path::Path) -> Result<(), String> {
    if is_remote(&path.to_string_lossy()) { Err(NO_FILE.to_owned()) } else { Ok(()) }
}

/// The path of a track only the server has.
pub fn song_path(song_id: &str) -> String {
    format!("{SONG_PREFIX}{song_id}")
}

/// The song a server-only track's path stands for.
pub fn song_of_path(path: &str) -> Option<&str> {
    path.strip_prefix(SONG_PREFIX)
}

/// The folder of an album only the server has (also its tracks' group folder).
pub fn album_folder(album_id: &str) -> String {
    format!("{ALBUM_PREFIX}{album_id}")
}

/// An artist the server credits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteArtist {
    pub name: String,
    pub mbid: Option<String>,
    pub sort_name: Option<String>,
}

/// One server album with its songs, as the app maps the client's model onto it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteAlbum {
    pub id: String,
    pub title: String,
    /// The album artist credit as shown (`displayArtist`, else `artist`).
    pub artist: String,
    /// Each album artist (OpenSubsonic `artists`), in order.
    pub artists: Vec<RemoteArtist>,
    /// ALBUMARTISTSORT, when the server says it.
    pub artist_sort: Option<String>,
    /// The album's date (its year, or a full date), its original release and this edition's
    /// release, as the server says them ("2004", "2004-03-15"); mapped as tags are
    /// (`tags::map_dates`).
    pub date: Option<String>,
    pub original_date: Option<String>,
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub label: Option<String>,
    /// MusicBrainz release types ("album", "live"...); the first is kept, as a tag's is.
    pub release_types: Vec<String>,
    pub compilation: bool,
    pub mb_album_id: Option<String>,
    /// When the server first had it, Unix milliseconds: "Recently added" for an album only the
    /// server has.
    pub created: Option<i64>,
    pub cover_art: Option<String>,
    /// The server's own user data: kept, not applied (see the module doc).
    pub starred: Option<i64>,
    /// Always None: ratings are no longer read from the server. Kept so the catalog's stored
    /// entries read and hash as before (a sync after the update rewrites only what the server
    /// had rated).
    pub rating: Option<u8>,
    pub play_count: Option<i64>,
    #[serde(skip)]
    pub songs: Vec<RemoteSong>,
}

/// One server song.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteSong {
    pub id: String,
    pub title: String,
    /// The artist credit as shown (`displayArtist`, else `artist`).
    pub artist: String,
    /// Each artist (OpenSubsonic `artists`), in order.
    pub artists: Vec<RemoteArtist>,
    /// The song's own album artist credit, when it differs from its album's.
    pub album_artist: Option<String>,
    pub album_artists: Vec<RemoteArtist>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    /// Seconds.
    pub duration: f64,
    /// The song's own date when its album has none.
    pub date: Option<String>,
    pub genres: Vec<String>,
    pub mb_recording_id: Option<String>,
    pub artist_sort: Option<String>,
    /// Unix milliseconds.
    pub created: Option<i64>,
    pub suffix: Option<String>,
    pub content_type: Option<String>,
    /// Bytes.
    pub size: i64,
    /// kbps.
    pub bitrate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub cover_art: Option<String>,
    /// The server's own user data: kept, not applied.
    pub starred: Option<i64>,
    /// Always None: ratings are no longer read from the server. Kept so the catalog's stored
    /// entries read and hash as before (a sync after the update rewrites only what the server
    /// had rated).
    pub rating: Option<u8>,
    pub play_count: Option<i64>,
    /// The file's path on the server, as it says it (relative to its music folder): with `size`,
    /// what tells the same file on disk (`file_key`).
    pub path: Option<String>,
}

/// What a sync is given.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub albums: Vec<RemoteAlbum>,
    /// The listing is the server's whole catalog (every page fetched, none failed): only then
    /// can what is not in it be removed.
    pub complete: bool,
    /// What the server said of its own last scan (kept, for the app to tell whether to sync).
    pub server_scan: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SyncOptions {
    /// Remove what the server no longer lists even where the shrink guard would keep it.
    pub confirm_removal: bool,
    /// Set to stop between batches: what is written stays, the next sync goes on from there.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Songs per transaction (default `BATCH_SONGS`).
    pub batch_songs: Option<usize>,
    /// Tests: stop after this many batches that wrote something, as `cancel` would.
    pub stop_after: Option<usize>,
}

/// Songs the server no longer lists, kept because too many went at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub missing: usize,
    pub known: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub albums: usize,
    pub songs: usize,
    /// Catalog entries new or changed since the last sync.
    pub changed_albums: usize,
    pub changed_songs: usize,
    pub removed_albums: usize,
    pub removed_songs: usize,
    /// Server songs matched to a local file (in this sync).
    pub attached: usize,
    /// Server-only tracks that became one with a local track.
    pub merged: usize,
    pub held: Option<Held>,
    /// Stopped (`SyncOptions::cancel`): what was written stays; nothing was removed.
    pub interrupted: bool,
    pub batches: usize,
    /// The longest transaction, in seconds.
    pub longest_write: f64,
    pub seconds: f64,
}

#[derive(Debug)]
pub enum SyncError {
    Db(rusqlite::Error),
    /// The library holds another server's catalog: `disconnect` first.
    OtherServer { connected: String },
}

impl From<rusqlite::Error> for SyncError {
    fn from(e: rusqlite::Error) -> Self {
        SyncError::Db(e)
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Db(e) => write!(f, "{e}"),
            SyncError::OtherServer { connected } => write!(f, "the library holds another server's music ({connected})"),
        }
    }
}

/// The server the library holds, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub identity: String,
    pub connected_at: i64,
    pub server_scan: Option<String>,
    /// Unix milliseconds of the last sync that saw the whole catalog.
    pub synced_at: Option<i64>,
    /// A sync began then and has not finished.
    pub sync_started: Option<i64>,
}

pub fn server(conn: &Connection) -> rusqlite::Result<Option<Server>> {
    conn.query_row("SELECT identity, connected_at, server_scan, synced_at, sync_started FROM remote_server WHERE id = 1", [], |r| {
        Ok(Server { identity: r.get(0)?, connected_at: r.get(1)?, server_scan: r.get(2)?, synced_at: r.get(3)?, sync_started: r.get(4)? })
    })
    .optional()
}

/// Whether a server is connected (its catalog is in the library, or being synced into it).
pub fn connected(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM remote_server)", [], |r| r.get(0))
}

/// Tracks by where they are, for Settings > About.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// A local file, not on the server.
    pub local: i64,
    /// On the server only.
    pub server: i64,
    /// A local file the server also has.
    pub both: i64,
}

pub fn counts(conn: &Connection) -> rusqlite::Result<Counts> {
    conn.query_row(
        "SELECT COALESCE(SUM(remote_id IS NULL), 0),
                COALESCE(SUM(remote_id IS NOT NULL AND path LIKE 'navidrome:%'), 0),
                COALESCE(SUM(remote_id IS NOT NULL AND path NOT LIKE 'navidrome:%'), 0)
         FROM tracks",
        [],
        |r| Ok(Counts { local: r.get(0)?, server: r.get(1)?, both: r.get(2)? }),
    )
}

/// What the app needs to stream a track from the server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    pub song_id: String,
    pub suffix: Option<String>,
    pub content_type: Option<String>,
    pub size: i64,
    pub bitrate_kbps: Option<u32>,
}

/// How to stream track `id` from the server: `None` for a track the server does not have.
pub fn stream_info(conn: &Connection, track_id: i64) -> rusqlite::Result<Option<StreamInfo>> {
    conn.query_row(
        "SELECT s.id, s.suffix, s.content_type, s.size, t.bitrate FROM tracks t JOIN remote_songs s ON s.id = t.remote_id WHERE t.id = ?1",
        [track_id],
        |r| Ok(StreamInfo {
            song_id: r.get(0)?,
            suffix: r.get(1)?,
            content_type: r.get(2)?,
            size: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            bitrate_kbps: r.get::<_, Option<u32>>(4)?.filter(|rate| *rate > 0),
        }),
    )
    .optional()
}

/// A song as the last sync saw it (the server's own user data with it).
pub fn song(conn: &Connection, song_id: &str) -> rusqlite::Result<Option<RemoteSong>> {
    let data: Option<String> = conn.query_row("SELECT data FROM remote_songs WHERE id = ?1", [song_id], |r| r.get(0)).optional()?;
    Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
}

/// What the server says of a song's MusicBrainz ids (its recording, its artists', its album's
/// release), as the last sync saw them: for a listen of a track only the server has.
pub fn musicbrainz_ids(conn: &Connection, song_id: &str) -> rusqlite::Result<crate::tags::MusicBrainzIds> {
    let row: Option<(String, String)> = conn.query_row("SELECT data, album_id FROM remote_songs WHERE id = ?1", [song_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let Some((data, album_id)) = row else { return Ok(Default::default()) };
    let song: Option<RemoteSong> = serde_json::from_str(&data).ok();
    let release: Option<String> = conn.query_row("SELECT mb_album_id FROM remote_albums WHERE id = ?1", [&album_id], |r| r.get(0)).optional()?.flatten();
    Ok(match song {
        Some(s) => crate::tags::MusicBrainzIds {
            recording: s.mb_recording_id.filter(|m| !m.trim().is_empty()),
            release: release.filter(|m| !m.trim().is_empty()),
            artists: s.artists.into_iter().filter_map(|a| a.mbid).filter(|m| !m.trim().is_empty()).collect(),
            ..Default::default()
        },
        None => Default::default(),
    })
}

/// An album as the last sync saw it (without its songs).
pub fn album(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<RemoteAlbum>> {
    let data: Option<String> = conn.query_row("SELECT data FROM remote_albums WHERE id = ?1", [album_id], |r| r.get(0)).optional()?;
    Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
}

// ---- identities ----------------------------------------------------------------

/// What an album artist is as one, for matching: "Various Artists" and its spellings are one.
fn artist_key(s: &str) -> String {
    if names::is_various(s) { "various artists".to_owned() } else { names::norm(s) }
}

/// The album artist's and title's identity a local copy is looked for by.
pub fn match_key(artist: &str, title: &str) -> String {
    format!("{}|{}", artist_key(artist), names::norm(title))
}

/// The key of an album only the server has: its release's, else its own.
fn own_key(a: &RemoteAlbum) -> String {
    match a.mb_album_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(mb) => format!("mb:{}", mb.to_ascii_lowercase()),
        None => format!("nd:{}", a.id),
    }
}

/// A sort name as a tag holds it: the one given, else the sort name of the only artist (as
/// ARTISTSORT names the one artist a track credits).
fn sort_of(given: &Option<String>, artists: &[RemoteArtist]) -> Option<String> {
    let given = given.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
    given.or_else(|| match artists {
        [one] => one.sort_name.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned),
        _ => None,
    })
}

fn year(d: Option<&str>) -> Option<&str> {
    d.and_then(|d| d.get(..4)).filter(|y| y.bytes().all(|b| b.is_ascii_digit()))
}

fn lower_mb(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase)
}

/// The format a server file is, as the library names it.
fn codec_of(suffix: Option<&str>, content_type: Option<&str>, bit_depth: Option<u8>) -> Codec {
    let s = suffix.unwrap_or("").to_ascii_lowercase();
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    match s.as_str() {
        "flac" => Codec::Flac,
        "mp3" => Codec::Mp3,
        "mp2" => Codec::Mp2,
        "opus" => Codec::Opus,
        "ogg" | "oga" if ct.contains("opus") => Codec::Opus,
        "ogg" | "oga" => Codec::Vorbis,
        "wav" => Codec::Wav,
        "aif" | "aiff" | "aifc" => Codec::Aiff,
        "alac" => Codec::Alac,
        "m4a" | "mp4" | "m4b" if bit_depth.is_some_and(|d| d > 0) => Codec::Alac,
        "m4a" | "mp4" | "m4b" | "aac" => Codec::Aac,
        _ => match ct.as_str() {
            "audio/flac" | "audio/x-flac" => Codec::Flac,
            "audio/mpeg" => Codec::Mp3,
            "audio/opus" => Codec::Opus,
            "audio/ogg" | "audio/vorbis" => Codec::Vorbis,
            "audio/wav" | "audio/x-wav" => Codec::Wav,
            "audio/aac" | "audio/mp4" => Codec::Aac,
            _ => Codec::Unknown,
        },
    }
}

// ---- the catalog as kept ---------------------------------------------------------

fn hash_of(parts: &[&str]) -> i64 {
    let mut h = crate::hash::Hasher::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(&[0]);
    }
    h.finish() as i64
}

struct Kept {
    album: RemoteAlbum,
    songs: Vec<KeptSong>,
}

struct KeptSong {
    /// What a server-only track of it was last written from (`placement`), if one was.
    placed: Option<i64>,
    song: RemoteSong,
}

/// What a server-only track is written from: its album's and its own entries, without the
/// server's user data (which is kept, not shown: a play counted on the server rewrites nothing).
fn placement(a: &RemoteAlbum, s: &RemoteSong) -> i64 {
    let a = RemoteAlbum { starred: None, rating: None, play_count: None, ..a.clone() };
    let s = RemoteSong { starred: None, rating: None, play_count: None, ..s.clone() };
    hash_of(&[&serde_json::to_string(&a).unwrap_or_default(), &serde_json::to_string(&s).unwrap_or_default()])
}

fn load_album(tx: &Connection, rid: &str) -> rusqlite::Result<Option<Kept>> {
    let Some(data) = tx.prepare_cached("SELECT data FROM remote_albums WHERE id = ?1")?.query_row([rid], |r| r.get::<_, String>(0)).optional()? else {
        return Ok(None);
    };
    let Ok(album) = serde_json::from_str::<RemoteAlbum>(&data) else { return Ok(None) };
    let mut st = tx.prepare_cached("SELECT placed, data FROM remote_songs WHERE album_id = ?1")?;
    let mut songs: Vec<KeptSong> = st
        .query_map([rid], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .filter_map(|(placed, d)| serde_json::from_str(&d).ok().map(|song| KeptSong { placed, song }))
        .collect();
    songs.sort_by(|a, b| {
        (a.song.disc.unwrap_or(1), a.song.track.unwrap_or(u32::MAX), &a.song.id).cmp(&(b.song.disc.unwrap_or(1), b.song.track.unwrap_or(u32::MAX), &b.song.id))
    });
    Ok(Some(Kept { album, songs }))
}

// ---- matching ----------------------------------------------------------------------

/// A local track of an album, as matching compares it.
struct LocalTrack {
    id: i64,
    disc: u32,
    track: Option<u32>,
    title: String,
    duration: f64,
    mb: Option<String>,
    /// Its file's `file_key`.
    file: Option<(i64, String)>,
}

/// What tells a file: its size in bytes and the last two components of its path (its folder and
/// its name), each as `names::norm` folds it, joined by "/". None without a size, for a path that
/// is no file's, or with fewer than two components (a bare file name says too little).
pub fn file_key(path: &str, size: i64) -> Option<(i64, String)> {
    if size <= 0 || is_remote(path) {
        return None;
    }
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.trim().is_empty()).collect();
    let [.., folder, name] = parts.as_slice() else { return None };
    // A drive ("C:") is no folder name.
    if folder.ends_with(':') {
        return None;
    }
    Some((size, format!("{}/{}", names::norm(folder), names::norm(name))))
}

/// A server song's `file_key`, from the path and size the server gives.
fn song_file(s: &RemoteSong) -> Option<(i64, String)> {
    file_key(s.path.as_deref()?, s.size)
}

/// Local files by `file_key`, each with the album it is in (built when first needed, once per
/// transaction).
#[derive(Default)]
struct LocalFiles {
    by_key: HashMap<(i64, String), Vec<i64>>,
}

impl LocalFiles {
    fn load(tx: &Connection) -> rusqlite::Result<Self> {
        let mut out = LocalFiles::default();
        let mut st = tx.prepare_cached(&format!("SELECT t.path, t.size, t.album_id FROM tracks t WHERE t.size > 0 AND {LOCAL_T}"))?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
            let (path, size, album) = row?;
            if let Some(k) = file_key(&path, size) {
                out.by_key.entry(k).or_default().push(album);
            }
        }
        Ok(out)
    }
}

/// Server songs by `file_key`, each with its server album (`reconcile`'s candidates).
#[derive(Default)]
struct RemoteFiles {
    by_key: HashMap<(i64, String), Vec<String>>,
}

impl RemoteFiles {
    fn load(tx: &Connection) -> rusqlite::Result<Self> {
        let mut out = RemoteFiles::default();
        let mut st = tx.prepare_cached("SELECT album_id, data FROM remote_songs WHERE size > 0")?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (rid, data) = row?;
            if let Some(k) = serde_json::from_str::<RemoteSong>(&data).ok().as_ref().and_then(song_file) {
                out.by_key.entry(k).or_default().push(rid);
            }
        }
        Ok(out)
    }
}

fn local_tracks(tx: &Connection, album: i64) -> rusqlite::Result<Vec<LocalTrack>> {
    let mut st = tx.prepare_cached(&format!(
        "SELECT t.id, t.disc_no, t.track_no, t.title, t.duration, g.mb_track_id, t.path, t.size FROM tracks t LEFT JOIN track_tags g ON g.track_id = t.id
         WHERE t.album_id = ?1 AND {LOCAL_T} ORDER BY t.id"
    ))?;
    st.query_map([album], |r| {
        Ok(LocalTrack {
            id: r.get(0)?,
            disc: r.get::<_, Option<u32>>(1)?.unwrap_or(1),
            track: r.get(2)?,
            title: r.get(3)?,
            duration: r.get(4)?,
            mb: lower_mb(r.get::<_, Option<String>>(5)?.as_deref()),
            file: file_key(&r.get::<_, String>(6)?, r.get::<_, Option<i64>>(7)?.unwrap_or(0)),
        })
    })?
    .collect()
}

fn title_key(s: &str) -> String {
    crate::fold::fold(&names::norm(s))
}

fn close(a: f64, b: f64) -> bool {
    a > 0.0 && b > 0.0 && (a - b).abs() <= 2.0
}

/// What `match_tracks` found.
#[derive(Debug, Default)]
struct Matching {
    /// (song index, local index).
    pairs: Vec<(usize, usize)>,
    /// Disc and track numbers where both have a track and none of them agree (title, duration).
    conflicts: usize,
}

/// Pair server songs with local tracks of one album (see the module doc). Each pass pairs only
/// what is the one candidate both ways.
fn match_tracks(local: &[LocalTrack], songs: &[&RemoteSong]) -> Matching {
    let mut out = Matching::default();
    let mut song_done = vec![false; songs.len()];
    let mut local_done = vec![false; local.len()];
    let song_mb: Vec<Option<String>> = songs.iter().map(|s| lower_mb(s.mb_recording_id.as_deref())).collect();
    let both_mb_differ = |si: usize, li: usize| matches!((&song_mb[si], &local[li].mb), (Some(a), Some(b)) if a != b);
    let titles: Vec<String> = songs.iter().map(|s| title_key(&s.title)).collect();
    let local_titles: Vec<String> = local.iter().map(|l| title_key(&l.title)).collect();
    // Unique both ways among `fits` (song, local) candidates of one pass.
    let pass = |fits: &dyn Fn(usize, usize) -> bool, song_done: &mut Vec<bool>, local_done: &mut Vec<bool>, out: &mut Matching| {
        let mut by_song: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut by_local: HashMap<usize, Vec<usize>> = HashMap::new();
        for si in (0..songs.len()).filter(|&i| !song_done[i]) {
            for li in (0..local.len()).filter(|&i| !local_done[i]) {
                if fits(si, li) {
                    by_song.entry(si).or_default().push(li);
                    by_local.entry(li).or_default().push(si);
                }
            }
        }
        let mut found: Vec<(usize, usize)> = by_song
            .into_iter()
            .filter_map(|(si, ls)| match ls.as_slice() {
                [li] if by_local.get(li).is_some_and(|s| s.len() == 1) => Some((si, *li)),
                _ => None,
            })
            .collect();
        found.sort_unstable();
        for (si, li) in found {
            song_done[si] = true;
            local_done[li] = true;
            out.pairs.push((si, li));
        }
    };
    let files: Vec<Option<(i64, String)>> = songs.iter().map(|s| song_file(s)).collect();
    pass(&|si, li| files[si].is_some() && files[si] == local[li].file && !both_mb_differ(si, li), &mut song_done, &mut local_done, &mut out);
    pass(&|si, li| song_mb[si].is_some() && song_mb[si] == local[li].mb, &mut song_done, &mut local_done, &mut out);
    let pos = |si: usize| songs[si].track.map(|t| (songs[si].disc.unwrap_or(1), t));
    let lpos = |li: usize| local[li].track.map(|t| (local[li].disc, t));
    let agree = |si: usize, li: usize| titles[si] == local_titles[li] || close(songs[si].duration, local[li].duration);
    pass(&|si, li| pos(si).is_some() && pos(si) == lpos(li) && !both_mb_differ(si, li) && agree(si, li), &mut song_done, &mut local_done, &mut out);
    pass(
        &|si, li| (pos(si).is_none() || lpos(li).is_none()) && !both_mb_differ(si, li) && titles[si] == local_titles[li] && close(songs[si].duration, local[li].duration),
        &mut song_done,
        &mut local_done,
        &mut out,
    );
    // A number both have whose tracks are plainly different tracks (tracks paired already, by
    // recording id or otherwise, are accounted for).
    for si in (0..songs.len()).filter(|&i| !song_done[i]) {
        let Some(p) = pos(si) else { continue };
        let at: Vec<usize> = (0..local.len()).filter(|&li| !local_done[li] && lpos(li) == Some(p)).collect();
        if !at.is_empty() && !at.iter().any(|&li| agree(si, li) && !both_mb_differ(si, li)) {
            out.conflicts += 1;
        }
    }
    out.pairs.sort_unstable();
    out
}

/// Whether album `id` has a track with a file.
fn has_local(tx: &Connection, id: i64) -> rusqlite::Result<bool> {
    tx.prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM tracks t WHERE t.album_id = ?1 AND {LOCAL_T})"))?.query_row([id], |r| r.get(0))
}

/// Whether album `id` is held by another server album still in the catalog.
fn held_by_other(tx: &Connection, id: i64, rid: &str) -> rusqlite::Result<bool> {
    let other: Option<String> = tx.prepare_cached("SELECT remote_id FROM albums WHERE id = ?1")?.query_row([id], |r| r.get(0)).optional()?.flatten();
    match other {
        Some(o) if o != rid => tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM remote_albums WHERE id = ?1)")?.query_row([o], |r| r.get(0)),
        _ => Ok(false),
    }
}

/// The local album whose files server album `k`'s songs are, when they are all one album's (see
/// the module doc).
fn by_files(tx: &Connection, rid: &str, k: &Kept, mb: Option<&str>, files: &mut Option<LocalFiles>) -> rusqlite::Result<Option<i64>> {
    let keys: Vec<(i64, String)> = k.songs.iter().filter_map(|s| song_file(&s.song)).collect();
    if keys.is_empty() {
        return Ok(None);
    }
    let index = match files {
        Some(f) => f,
        None => files.insert(LocalFiles::load(tx)?),
    };
    let mut albums: HashSet<i64> = HashSet::new();
    let mut matched = 0;
    for key in &keys {
        match index.by_key.get(key).map(Vec::as_slice) {
            None | Some([]) => {}
            Some([one]) => {
                albums.insert(*one);
                matched += 1;
            }
            // Two local files alike (copies in two folders of one name): no sign either way.
            Some(_) => return Ok(None),
        }
    }
    let [album] = albums.into_iter().collect::<Vec<_>>()[..] else { return Ok(None) };
    if !has_local(tx, album)? || held_by_other(tx, album, rid)? {
        return Ok(None);
    }
    let key: Option<String> = tx.prepare_cached("SELECT key FROM albums WHERE id = ?1")?.query_row([album], |r| r.get(0)).optional()?;
    if let (Some(theirs), Some(ours)) = (key.as_deref().and_then(|k| k.strip_prefix("mb:")), mb)
        && theirs != ours
    {
        return Ok(None);
    }
    let local = local_tracks(tx, album)?;
    let songs: Vec<&RemoteSong> = k.songs.iter().map(|s| &s.song).collect();
    let m = match_tracks(&local, &songs);
    Ok((m.conflicts == 0 && matched * 2 >= local.len().min(songs.len())).then_some(album))
}

/// The local album server album `k` is, if one plainly is (see the module doc).
fn find_local_album(tx: &Connection, rid: &str, k: &Kept, files: &mut Option<LocalFiles>) -> rusqlite::Result<Option<i64>> {
    let a = &k.album;
    let mb = lower_mb(a.mb_album_id.as_deref());
    if let Some(mb) = &mb
        && let Some(id) = tx.prepare_cached("SELECT id FROM albums WHERE key = ?1")?.query_row([format!("mb:{mb}")], |r| r.get::<_, i64>(0)).optional()?
        && has_local(tx, id)?
        && !held_by_other(tx, id, rid)?
    {
        return Ok(Some(id));
    }
    if let Some(id) = by_files(tx, rid, k, mb.as_deref(), files)? {
        return Ok(Some(id));
    }
    let want = match_key(&a.artist, &a.title);
    let mut ids: Vec<i64> = Vec::new();
    if let Some(first) = names::split_artists(&a.artist).first() {
        let mut st = tx.prepare_cached("SELECT DISTINCT album_id FROM album_artists WHERE norm = ?1 AND position = 0")?;
        ids.extend(st.query_map([names::norm(first)], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    if a.compilation || names::is_various(&a.artist) {
        let mut st = tx.prepare_cached("SELECT id FROM albums WHERE compilation = 1")?;
        ids.extend(st.query_map([], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    ids.sort_unstable();
    ids.dedup();
    let (date, _, _) = tags::map_dates(a.date.clone(), a.original_date.clone(), a.release_date.clone(), None);
    let songs: Vec<&RemoteSong> = k.songs.iter().map(|s| &s.song).collect();
    let mut passed = Vec::new();
    for id in ids {
        let Some((key, title, artist, adate)) = tx
            .prepare_cached("SELECT key, title, album_artist, date FROM albums WHERE id = ?1")?
            .query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?)))
            .optional()?
        else {
            continue;
        };
        if match_key(&artist, &title) != want || !has_local(tx, id)? || held_by_other(tx, id, rid)? {
            continue;
        }
        if let (Some(theirs), Some(ours)) = (key.strip_prefix("mb:"), &mb)
            && theirs != ours
        {
            continue;
        }
        if let (Some(y1), Some(y2)) = (year(adate.as_deref()), year(date.as_deref()))
            && y1 != y2
        {
            continue;
        }
        let local = local_tracks(tx, id)?;
        let m = match_tracks(&local, &songs);
        if m.conflicts == 0 && !m.pairs.is_empty() && m.pairs.len() * 2 >= local.len().min(songs.len()) {
            passed.push(id);
        }
    }
    Ok(match passed.as_slice() {
        [one] => Some(*one),
        _ => None,
    })
}

// ---- placing ---------------------------------------------------------------------

/// What the placing of albums changed, to finish in the same transaction.
#[derive(Default)]
struct Work {
    /// The local files by `file_key`, once an album with paths asked (`by_files`).
    files: Option<LocalFiles>,
    touched: Touched,
    /// Albums whose values (or existence) are to be worked out again.
    affected: HashSet<i64>,
    attached: usize,
    merged: usize,
}

/// A track has user data of its own: plays, a favourite, a playlist place, a place in the queue.
fn has_history(tx: &Connection, id: i64) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM plays WHERE track_id = ?1) OR EXISTS(SELECT 1 FROM favourite_tracks WHERE track_id = ?1)
             OR EXISTS(SELECT 1 FROM playlist_items WHERE track_id = ?1)
             OR EXISTS(SELECT 1 FROM queue_items WHERE track_id = ?1)",
    )?
    .query_row([id], |r| r.get(0))
}

/// Move a track's user data to another (every table that names a track: plays, favourites,
/// playlist items, the queue). A favourite keeps its earliest time.
fn move_history(tx: &Connection, from: i64, to: i64) -> rusqlite::Result<()> {
    tx.prepare_cached("UPDATE plays SET track_id = ?2 WHERE track_id = ?1")?.execute(params![from, to])?;
    tx.prepare_cached(
        "INSERT INTO favourite_tracks (track_id, added_at) SELECT ?2, added_at FROM favourite_tracks WHERE track_id = ?1
         ON CONFLICT(track_id) DO UPDATE SET added_at = MIN(added_at, excluded.added_at)",
    )?
    .execute(params![from, to])?;
    tx.prepare_cached("DELETE FROM favourite_tracks WHERE track_id = ?1")?.execute([from])?;
    tx.prepare_cached("UPDATE playlist_items SET track_id = ?2 WHERE track_id = ?1")?.execute(params![from, to])?;
    tx.prepare_cached("UPDATE queue_items SET track_id = ?2 WHERE track_id = ?1")?.execute(params![from, to])?;
    Ok(())
}

fn delete_track(tx: &Connection, id: i64, w: &mut Work) -> rusqlite::Result<()> {
    if let Some(album) = tx.prepare_cached("SELECT album_id FROM tracks WHERE id = ?1")?.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
        w.affected.insert(album);
    }
    relations::forget_track(tx, id, &mut w.touched)?;
    tx.prepare_cached("DELETE FROM tracks WHERE id = ?1")?.execute([id])?;
    Ok(())
}

/// A server-only track (`server`) and a local track (`local`) are one: keep the row with the
/// history (the server's, when only it has some, or when both have and it is the older), the
/// other's history moved to it, and make it the local track (its file, tags, credits), with
/// `remote_id` set to `song`.
fn merge(tx: &Connection, song: &str, server: i64, local: i64, w: &mut Work) -> rusqlite::Result<()> {
    w.merged += 1;
    let keep_server = has_history(tx, server)? && (!has_history(tx, local)? || server < local);
    if let Some(album) = tx.prepare_cached("SELECT album_id FROM tracks WHERE id = ?1")?.query_row([server], |r| r.get::<_, i64>(0)).optional()? {
        w.affected.insert(album);
    }
    if !keep_server {
        move_history(tx, server, local)?;
        delete_track(tx, server, w)?;
        tx.prepare_cached("UPDATE tracks SET remote_id = ?2 WHERE id = ?1")?.execute(params![local, song])?;
        return Ok(());
    }
    // The server's row takes the local track's place: its history stays, the local one's comes.
    move_history(tx, local, server)?;
    relations::forget_track(tx, server, &mut w.touched)?;
    for table in ["track_tags", "track_artists", "track_genres"] {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE track_id = ?1"))?.execute([server])?;
        tx.prepare_cached(&format!("UPDATE {table} SET track_id = ?2 WHERE track_id = ?1"))?.execute(params![local, server])?;
    }
    const COLS: &str = "path, album_id, title, artist, disc_no, track_no, duration, genre, mtime, size, added_at, codec, sample_rate, bit_depth, bitrate, channels, tags_read, from_name";
    let row: Vec<rusqlite::types::Value> = tx.query_row(&format!("SELECT {COLS} FROM tracks WHERE id = ?1"), [local], |r| (0..18).map(|i| r.get(i)).collect())?;
    tx.prepare_cached("DELETE FROM tracks WHERE id = ?1")?.execute([local])?;
    let sets: Vec<String> = COLS.split(", ").enumerate().map(|(i, c)| format!("{c} = ?{}", i + 2)).collect();
    let mut values: Vec<rusqlite::types::Value> = vec![server.into()];
    values.extend(row);
    tx.prepare_cached(&format!("UPDATE tracks SET {}, remote_id = NULL WHERE id = ?1", sets.join(", ")))?.execute(rusqlite::params_from_iter(values))?;
    tx.prepare_cached("UPDATE tracks SET remote_id = ?2 WHERE id = ?1")?.execute(params![server, song])?;
    let album: i64 = tx.query_row("SELECT album_id FROM tracks WHERE id = ?1", [server], |r| r.get(0))?;
    w.affected.insert(album);
    // Its credits were the local track's (moved above): their artists need settling again.
    for a in tx.prepare_cached("SELECT norm, artist_id FROM track_artists WHERE track_id = ?1")?.query_map([server], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (n, id) = a?;
        w.touched.norms.insert(n);
        w.touched.artists.insert(id);
    }
    for g in tx.prepare_cached("SELECT genre_id FROM track_genres WHERE track_id = ?1")?.query_map([server], |r| r.get::<_, i64>(0))? {
        w.touched.genres.insert(g?);
    }
    tx.prepare_cached("UPDATE remote_songs SET placed = NULL WHERE id = ?1")?.execute([song])?;
    Ok(())
}

/// Write a song as a server-only track of `album`: a new row (`row` None) or `row` rewritten,
/// with its tags, credits and genres as the scanner writes a file's.
fn write_song(tx: &Connection, row: Option<i64>, album: i64, a: &RemoteAlbum, k: &KeptSong, w: &mut Work) -> rusqlite::Result<i64> {
    let s = &k.song;
    let album_artist = s.album_artist.clone().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| a.artist.clone());
    let structured = if s.album_artist.is_some() && !s.album_artists.is_empty() { &s.album_artists } else { &a.artists };
    let album_artist_mbids = {
        let ids: Vec<String> = structured.iter().filter_map(|x| lower_mb(x.mbid.as_deref())).collect();
        (!ids.is_empty() && ids.len() == structured.len() && ids.len() == names::split_artists(&album_artist).len()).then(|| ids.join(";"))
    };
    let artist_values: Vec<String> = if s.artist.trim().is_empty() {
        if s.artists.is_empty() { vec![album_artist.clone()] } else { vec![s.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join("; ")] }
    } else {
        vec![names::tidy(&s.artist)]
    };
    let listed: Vec<String> = s.artists.iter().map(|x| names::tidy(&x.name)).filter(|n| !n.is_empty()).collect();
    let ids: Vec<String> = s.artists.iter().filter_map(|x| lower_mb(x.mbid.as_deref())).collect();
    let ids = if ids.len() == listed.len() { ids } else { Vec::new() };
    let title = names::tidy(&s.title);
    let credits: Vec<Credit> = tags::credits_of(&artist_values, &listed, ids, Some(&title));
    let (genre, genres) = tags::genres_of(&if s.genres.is_empty() { a.genres.clone() } else { s.genres.clone() });
    let (date, original, release) = tags::map_dates(a.date.clone().or_else(|| s.date.clone()), a.original_date.clone(), a.release_date.clone(), None);
    let codec = codec_of(s.suffix.as_deref(), s.content_type.as_deref(), s.bit_depth);
    let artist = artist_values.join("; ");
    let added = s.created.or(a.created).map_or_else(now, |ms| ms / 1000);
    let path = song_path(&s.id);
    let duration = if s.duration.is_finite() && s.duration > 0.0 { s.duration } else { 0.0 };
    let id = match row {
        Some(id) => {
            tx.prepare_cached(
                "UPDATE tracks SET album_id=?2, title=?3, artist=?4, disc_no=?5, track_no=?6, duration=?7, genre=?8, mtime=0, size=?9, path=?10,
                 codec=?11, sample_rate=?12, bit_depth=?13, bitrate=?14, channels=?15, tags_read=?16, from_name=0, remote_id=?17 WHERE id=?1",
            )?
            .execute(params![
                id, album, title, artist, s.disc, s.track, duration, genre, s.size, path, codec.name(), s.sample_rate, s.bit_depth, s.bitrate, s.channels,
                scan::TAGS_VERSION, s.id
            ])?;
            id
        }
        None => {
            tx.prepare_cached(
                "INSERT INTO tracks (album_id, title, artist, disc_no, track_no, duration, genre, mtime, size, path, codec, sample_rate, bit_depth,
                 bitrate, channels, tags_read, from_name, added_at, remote_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16, ?17)",
            )?
            .execute(params![
                album, title, artist, s.disc, s.track, duration, genre, s.size, path, codec.name(), s.sample_rate, s.bit_depth, s.bitrate, s.channels,
                scan::TAGS_VERSION, added, s.id
            ])?;
            tx.last_insert_rowid()
        }
    };
    tx.prepare_cached(
        "INSERT OR REPLACE INTO track_tags (track_id, group_folder, album_title, album_artist, album_artist_mbids, album_artist_sort, artist_sort,
         date, original_date, release_date, label, release_type, mb_album_id, compilation, mb_track_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?
    .execute(params![
        id,
        album_folder(&a.id),
        Some(names::tidy(&a.title)).filter(|t| !t.is_empty()),
        Some(names::tidy(&album_artist)).filter(|t| !t.is_empty()),
        album_artist_mbids,
        sort_of(&a.artist_sort, &a.artists),
        sort_of(&s.artist_sort, &s.artists),
        date,
        original,
        release,
        a.label.as_deref().map(names::tidy).filter(|l| !l.is_empty()),
        a.release_types.first().cloned(),
        lower_mb(a.mb_album_id.as_deref()),
        a.compilation,
        lower_mb(s.mb_recording_id.as_deref()),
    ])?;
    relations::set_track_artists(tx, id, &credits, &mut w.touched)?;
    relations::set_track_genres(tx, id, &genres, &mut w.touched)?;
    tx.prepare_cached("UPDATE remote_songs SET placed = ?2 WHERE id = ?1")?.execute(params![s.id, placement(a, s)])?;
    w.affected.insert(album);
    Ok(id)
}

/// The album row server album `a` shows as when no local album is it: the one it has, else the
/// row of its key, else a new one (dated by the server: `added_source` "server").
fn own_album(tx: &Connection, rid: &str, a: &RemoteAlbum, current: Option<i64>) -> rusqlite::Result<i64> {
    let key = own_key(a);
    let free = |id: i64| -> rusqlite::Result<bool> { Ok(!has_local(tx, id)? && !held_by_other(tx, id, rid)?) };
    let key_of = |k: &str| -> rusqlite::Result<Option<i64>> { tx.prepare_cached("SELECT id FROM albums WHERE key = ?1")?.query_row([k], |r| r.get(0)).optional() };
    if let Some(c) = current
        && !has_local(tx, c)?
    {
        return Ok(c);
    }
    let fallback = format!("nd:{}", a.id);
    for k in [key.as_str(), fallback.as_str()] {
        match key_of(k)? {
            Some(id) if free(id)? => return Ok(id),
            Some(_) => continue,
            None => {
                let at = a.created.unwrap_or_else(now_ms);
                tx.prepare_cached(
                    "INSERT INTO albums (key, title, album_artist, folder, added_at, first_seen, added_source) VALUES (?1, '', '', ?2, ?3, ?4, 'server')",
                )?
                .execute(params![k, album_folder(&a.id), at, now_ms()])?;
                return Ok(tx.last_insert_rowid());
            }
        }
    }
    // Both keys are taken by albums with files that are not this one: keyed apart.
    let k = format!("nd:{}:{}", a.id, now_ms());
    tx.prepare_cached("INSERT INTO albums (key, title, album_artist, folder, added_at, first_seen, added_source) VALUES (?1, '', '', ?2, ?3, ?4, 'server')")?.execute(
        params![k, album_folder(&a.id), a.created.unwrap_or_else(now_ms), now_ms()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Show server album `rid` in the library as its catalog says (`remote_albums`): in the local
/// album it is, if one plainly is (its songs matched to the local tracks, the rest server-only
/// tracks of that album), else as an album of its own. Idempotent: nothing is written for what
/// already is as it should be.
fn place(tx: &Connection, rid: &str, w: &mut Work) -> rusqlite::Result<()> {
    let current: Option<i64> = tx.prepare_cached("SELECT id FROM albums WHERE remote_id = ?1")?.query_row([rid], |r| r.get(0)).optional()?;
    let Some(k) = load_album(tx, rid)? else {
        if let Some(c) = current {
            tx.prepare_cached("UPDATE albums SET remote_id = NULL, remote_art = NULL WHERE id = ?1")?.execute([c])?;
            w.affected.insert(c);
        }
        return Ok(());
    };
    // An album the server lists with no songs is not shown (an album of its own would be empty).
    if k.songs.is_empty() {
        if let Some(c) = current {
            tx.prepare_cached("UPDATE albums SET remote_id = NULL, remote_art = NULL WHERE id = ?1")?.execute([c])?;
            w.affected.insert(c);
        }
        return Ok(());
    }
    let target = find_local_album(tx, rid, &k, &mut w.files)?;
    let album = match target {
        Some(l) => l,
        None => own_album(tx, rid, &k.album, current)?,
    };
    if let Some(c) = current
        && c != album
    {
        tx.prepare_cached("UPDATE albums SET remote_id = NULL, remote_art = NULL WHERE id = ?1")?.execute([c])?;
        w.affected.insert(c);
        // An album of its own that is now part of a local one passes its favourite on.
        if !has_local(tx, c)? {
            tx.prepare_cached(
                "INSERT INTO favourite_albums (album_id, added_at) SELECT ?2, added_at FROM favourite_albums WHERE album_id = ?1
                 ON CONFLICT(album_id) DO UPDATE SET added_at = MIN(added_at, excluded.added_at)",
            )?
            .execute(params![c, album])?;
        }
    }
    let (has, art): (Option<String>, Option<String>) =
        tx.prepare_cached("SELECT remote_id, remote_art FROM albums WHERE id = ?1")?.query_row([album], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if has.as_deref() != Some(rid) || art != k.album.cover_art {
        // Another server album that held it (gone from the catalog, or not this one any more)
        // lets go first: one album row per server album.
        tx.prepare_cached("UPDATE albums SET remote_id = ?2, remote_art = ?3 WHERE id = ?1")?.execute(params![album, rid, k.album.cover_art])?;
        w.affected.insert(album);
    }
    // An album of its own: its key follows the catalog (a release id added on the server).
    if target.is_none() {
        let key = own_key(&k.album);
        let has_key: String = tx.query_row("SELECT key FROM albums WHERE id = ?1", [album], |r| r.get(0))?;
        if has_key != key && !has_key.starts_with("t:") && tx.prepare_cached("SELECT id FROM albums WHERE key = ?1")?.query_row([&key], |r| r.get::<_, i64>(0)).optional()?.is_none() {
            tx.prepare_cached("UPDATE albums SET key = ?2 WHERE id = ?1")?.execute(params![album, key])?;
        }
    }
    let songs: Vec<&RemoteSong> = k.songs.iter().map(|s| &s.song).collect();
    let local = if target.is_some() { local_tracks(tx, album)? } else { Vec::new() };
    let pairs: HashMap<usize, i64> = match_tracks(&local, &songs).pairs.into_iter().map(|(si, li)| (si, local[li].id)).collect();
    for (si, ks) in k.songs.iter().enumerate() {
        let holder: Option<(i64, String, i64)> = tx
            .prepare_cached("SELECT id, path, album_id FROM tracks WHERE remote_id = ?1")?
            .query_row([&ks.song.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        match (pairs.get(&si).copied(), holder) {
            (Some(tid), Some((h, _, _))) if h == tid => {}
            (Some(tid), Some((h, path, _))) if is_remote(&path) => merge(tx, &ks.song.id, h, tid, w)?,
            (Some(tid), holder) => {
                if let Some((h, _, a)) = holder {
                    tx.prepare_cached("UPDATE tracks SET remote_id = NULL WHERE id = ?1")?.execute([h])?;
                    w.affected.insert(a);
                }
                tx.prepare_cached("UPDATE tracks SET remote_id = ?2 WHERE id = ?1")?.execute(params![tid, ks.song.id])?;
                tx.prepare_cached("UPDATE remote_songs SET placed = NULL WHERE id = ?1")?.execute([&ks.song.id])?;
                w.attached += 1;
                w.affected.insert(album);
            }
            (None, Some((h, path, a))) if is_remote(&path) => {
                if a != album || ks.placed != Some(placement(&k.album, &ks.song)) {
                    w.affected.insert(a);
                    write_song(tx, Some(h), album, &k.album, ks, w)?;
                }
            }
            (None, holder) => {
                if let Some((h, _, a)) = holder {
                    tx.prepare_cached("UPDATE tracks SET remote_id = NULL WHERE id = ?1")?.execute([h])?;
                    w.affected.insert(a);
                }
                write_song(tx, None, album, &k.album, ks, w)?;
            }
        }
    }
    Ok(())
}

/// Work out again the values of the albums `w` touched (from their local tracks when they have
/// any, else from the server's: `scan::album_values`), delete the ones left empty, and settle
/// the artists' and genres' credits (their names are left to `settle`).
fn finish(tx: &Connection, w: &mut Work) -> rusqlite::Result<()> {
    let mut ids: Vec<i64> = w.affected.drain().collect();
    ids.sort_unstable();
    for id in ids {
        let has_tracks: bool = tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM tracks WHERE album_id = ?1)")?.query_row([id], |r| r.get(0))?;
        if has_tracks {
            scan::album_values(tx, id, &mut w.touched)?;
        } else if tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM albums WHERE id = ?1)")?.query_row([id], |r| r.get::<_, bool>(0))? {
            relations::forget_album(tx, id, &mut w.touched)?;
            tx.prepare_cached("DELETE FROM albums WHERE id = ?1")?.execute([id])?;
        }
    }
    relations::resolve_names(tx, &mut w.touched)
}

/// Artists and genres whose names are still to be worked out, settled a few at a time in
/// transactions of their own (`relations::settle_artists_and_genres`).
#[derive(Default)]
struct Unsettled {
    artists: HashSet<i64>,
    genres: HashSet<i64>,
    last: Option<Instant>,
}

impl Unsettled {
    fn take(&mut self, w: &mut Work) {
        self.artists.extend(w.touched.artists.drain());
        self.genres.extend(w.touched.genres.drain());
    }

    fn settle(&mut self, conn: &mut Connection, all: bool) -> rusqlite::Result<()> {
        if !all && self.last.is_some_and(|t| t.elapsed() < SETTLE_EVERY) {
            return Ok(());
        }
        self.last = Some(Instant::now());
        let mut artists: Vec<i64> = self.artists.drain().collect();
        let mut genres: Vec<i64> = self.genres.drain().collect();
        artists.sort_unstable();
        genres.sort_unstable();
        for chunk in artists.chunks(SETTLE_CHUNK) {
            crate::db::let_waiting_in();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            relations::settle_artists_and_genres(&tx, chunk, &[])?;
            tx.commit()?;
        }
        for chunk in genres.chunks(SETTLE_CHUNK) {
            crate::db::let_waiting_in();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            relations::settle_artists_and_genres(&tx, &[], chunk)?;
            tx.commit()?;
        }
        Ok(())
    }
}

// ---- sync ------------------------------------------------------------------------

/// Bring the library in step with the server's catalog (see the module doc): the catalog kept,
/// every album new or changed since the last sync (or not yet shown) placed, a batch of albums
/// per transaction; then, when the listing was complete and plausible, what the server no longer
/// has removed; then `reconcile`. `identity` names the server (its URL and user): the library
/// holds one server's catalog at a time (`SyncError::OtherServer`: `disconnect` first).
pub fn sync(conn: &mut Connection, identity: &str, catalog: &Catalog, opts: &SyncOptions) -> Result<SyncReport, SyncError> {
    let started = Instant::now();
    let mut report = SyncReport { albums: catalog.albums.len(), songs: catalog.albums.iter().map(|a| a.songs.len()).sum(), ..Default::default() };
    match server(conn)? {
        Some(s) if s.identity != identity => return Err(SyncError::OtherServer { connected: s.identity }),
        Some(_) => {
            conn.execute("UPDATE remote_server SET sync_started = COALESCE(sync_started, ?1) WHERE id = 1", [now_ms()])?;
        }
        None => {
            conn.execute("INSERT INTO remote_server (id, identity, connected_at, sync_started) VALUES (1, ?1, ?2, ?2)", params![identity, now_ms()])?;
        }
    }
    let never = AtomicBool::new(false);
    let cancel: &AtomicBool = opts.cancel.as_deref().unwrap_or(&never);
    let cap = opts.batch_songs.unwrap_or(BATCH_SONGS).max(1);
    let mut unsettled = Unsettled::default();
    // A song listed under two albums (a server's inconsistency) is kept under the first that
    // lists it: under each in turn, it would be written over and placed again at every sync.
    let mut first: HashMap<&str, &str> = HashMap::new();
    for a in &catalog.albums {
        for s in &a.songs {
            first.entry(s.id.as_str()).or_insert(a.id.as_str());
        }
    }
    let mut at = 0;
    let mut wrote = 0;
    while at < catalog.albums.len() {
        if cancel.load(Ordering::Relaxed) || opts.stop_after.is_some_and(|n| wrote >= n) {
            report.interrupted = true;
            break;
        }
        let mut end = at;
        let mut n = 0;
        while end < catalog.albums.len() && (end == at || n + catalog.albums[end].songs.len() <= cap) {
            n += catalog.albums[end].songs.len();
            end += 1;
        }
        let batch = &catalog.albums[at..end];
        at = end;
        crate::db::let_waiting_in();
        let t0 = Instant::now();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut w = Work::default();
        let mut any = false;
        for a in batch {
            let (changed_album, changed_songs) = keep(&tx, a, &first)?;
            report.changed_albums += usize::from(changed_album);
            report.changed_songs += changed_songs;
            let placed: bool = tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM albums WHERE remote_id = ?1)")?.query_row([&a.id], |r| r.get(0))?;
            if changed_album || changed_songs > 0 || !placed {
                place(&tx, &a.id, &mut w)?;
                any = true;
            }
        }
        wrote += usize::from(any);
        finish(&tx, &mut w)?;
        tx.commit()?;
        report.batches += 1;
        report.longest_write = report.longest_write.max(t0.elapsed().as_secs_f64());
        report.attached += w.attached;
        report.merged += w.merged;
        unsettled.take(&mut w);
        unsettled.settle(conn, false)?;
    }
    if !report.interrupted && catalog.complete {
        remove_missing(conn, catalog, opts.confirm_removal, &mut report, &mut unsettled)?;
    }
    if !report.interrupted {
        conn.execute(
            "UPDATE remote_server SET server_scan = ?1, sync_started = NULL, synced_at = CASE WHEN ?2 THEN ?3 ELSE synced_at END WHERE id = 1",
            params![catalog.server_scan, catalog.complete && report.held.is_none(), now_ms()],
        )?;
    }
    unsettled.settle(conn, true)?;
    let r = reconcile(conn)?;
    report.attached += r.attached;
    report.merged += r.merged;
    let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()));
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Keep album `a` and its songs as the catalog lists them, writing only what changed; a song
/// `first` gives to an album before it in the catalog is left to that one. Returns whether the
/// album changed (or is new), and how many of its songs did (new, changed, moved to it, or gone
/// from it).
fn keep(tx: &Connection, a: &RemoteAlbum, first: &HashMap<&str, &str>) -> rusqlite::Result<(bool, usize)> {
    let data = serde_json::to_string(a).unwrap_or_default();
    let hash = hash_of(&[&data]);
    let stored: Option<i64> = tx.prepare_cached("SELECT hash FROM remote_albums WHERE id = ?1")?.query_row([&a.id], |r| r.get(0)).optional()?;
    let changed_album = stored != Some(hash);
    if changed_album {
        tx.prepare_cached("INSERT OR REPLACE INTO remote_albums (id, hash, match_key, mb_album_id, data) VALUES (?1, ?2, ?3, ?4, ?5)")?.execute(params![
            a.id,
            hash,
            match_key(&a.artist, &a.title),
            lower_mb(a.mb_album_id.as_deref()),
            data
        ])?;
    }
    let mut changed = 0;
    let mut listed: HashSet<&str> = HashSet::new();
    for s in &a.songs {
        if first.get(s.id.as_str()).is_some_and(|f| *f != a.id) {
            continue;
        }
        listed.insert(&s.id);
        let data = serde_json::to_string(s).unwrap_or_default();
        let hash = hash_of(&[&a.id, &data]);
        let stored: Option<i64> = tx.prepare_cached("SELECT hash FROM remote_songs WHERE id = ?1")?.query_row([&s.id], |r| r.get(0)).optional()?;
        if stored != Some(hash) {
            tx.prepare_cached(
                "INSERT INTO remote_songs (id, album_id, hash, placed, suffix, content_type, size, data) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET album_id = excluded.album_id, hash = excluded.hash, suffix = excluded.suffix,
                     content_type = excluded.content_type, size = excluded.size, data = excluded.data",
            )?
            .execute(params![s.id, a.id, hash, s.suffix, s.content_type, s.size, data])?;
            changed += 1;
        }
    }
    // Songs kept under this album that it no longer lists (moved to another, or gone): placed
    // again with the album that lists them, or removed at the end.
    let mut st = tx.prepare_cached("SELECT id FROM remote_songs WHERE album_id = ?1")?;
    let kept: Vec<String> = st.query_map([&a.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    changed += kept.iter().filter(|id| !listed.contains(id.as_str())).count();
    Ok((changed_album, changed))
}

/// What the server no longer lists, removed (see the module doc for the guard), a batch at a time.
fn remove_missing(conn: &mut Connection, catalog: &Catalog, confirm: bool, report: &mut SyncReport, unsettled: &mut Unsettled) -> rusqlite::Result<()> {
    let listed_songs: HashSet<&str> = catalog.albums.iter().flat_map(|a| a.songs.iter().map(|s| s.id.as_str())).collect();
    let listed_albums: HashSet<&str> = catalog.albums.iter().map(|a| a.id.as_str()).collect();
    let kept_songs: Vec<String> = conn.prepare("SELECT id FROM remote_songs ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let kept_albums: Vec<String> = conn.prepare("SELECT id FROM remote_albums ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let gone_songs: Vec<&String> = kept_songs.iter().filter(|s| !listed_songs.contains(s.as_str())).collect();
    let gone_albums: Vec<&String> = kept_albums.iter().filter(|a| !listed_albums.contains(a.as_str())).collect();
    let known = kept_songs.len();
    let found = known - gone_songs.len();
    if !confirm && gone_songs.len() >= scan::GUARD_MIN_MISSING && found * scan::GUARD_FOUND_RATIO < known {
        log::warn!("server sync: {} of {known} songs not listed; kept until confirmed", gone_songs.len());
        report.held = Some(Held { missing: gone_songs.len(), known });
        return Ok(());
    }
    for chunk in gone_songs.chunks(BATCH_SONGS) {
        crate::db::let_waiting_in();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut w = Work::default();
        for id in chunk {
            forget_song(&tx, id, &mut w)?;
        }
        finish(&tx, &mut w)?;
        tx.commit()?;
        report.batches += 1;
        unsettled.take(&mut w);
        unsettled.settle(conn, false)?;
    }
    for chunk in gone_albums.chunks(ALBUMS_PER_TX) {
        crate::db::let_waiting_in();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut w = Work::default();
        for id in chunk {
            // Its songs are gone (above) or listed under another album now.
            tx.prepare_cached("DELETE FROM remote_albums WHERE id = ?1")?.execute([id])?;
            place(&tx, id, &mut w)?;
        }
        finish(&tx, &mut w)?;
        tx.commit()?;
        unsettled.take(&mut w);
    }
    report.removed_songs = gone_songs.len();
    report.removed_albums = gone_albums.len();
    Ok(())
}

/// A song leaves the library's view of the server: its server-only track goes (with its plays,
/// as a deleted file's do), a local track only loses its `remote_id`.
fn forget_song(tx: &Connection, id: &str, w: &mut Work) -> rusqlite::Result<()> {
    let holder: Option<(i64, String, i64)> =
        tx.prepare_cached("SELECT id, path, album_id FROM tracks WHERE remote_id = ?1")?.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    if let Some((t, path, album)) = holder {
        if is_remote(&path) {
            delete_track(tx, t, w)?;
        } else {
            tx.prepare_cached("UPDATE tracks SET remote_id = NULL WHERE id = ?1")?.execute([t])?;
            w.affected.insert(album);
        }
    }
    tx.prepare_cached("DELETE FROM remote_songs WHERE id = ?1")?.execute([id])?;
    tx.prepare_cached("DELETE FROM remote_marks WHERE song_id = ?1")?.execute([id])?;
    tx.prepare_cached("DELETE FROM remote_plays WHERE song_id = ?1")?.execute([id])?;
    Ok(())
}

// ---- after a scan ------------------------------------------------------------------

/// What `reconcile` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Server albums placed again.
    pub albums: usize,
    pub attached: usize,
    pub merged: usize,
}

/// Albums a scan wrote, for the next `reconcile` (only while a server is connected).
pub(crate) fn mark_pending(tx: &Connection, albums: impl IntoIterator<Item = i64>) -> rusqlite::Result<()> {
    if !connected(tx)? {
        return Ok(());
    }
    let mut st = tx.prepare_cached("INSERT OR IGNORE INTO remote_pending (album_id) VALUES (?1)")?;
    for a in albums {
        st.execute([a])?;
    }
    Ok(())
}

/// The server albums that may be, or may have been, local album `id`.
fn candidates(tx: &Connection, id: i64, out: &mut Vec<String>, files: &mut Option<RemoteFiles>) -> rusqlite::Result<()> {
    let row: Option<(String, String, String, Option<String>)> = tx
        .prepare_cached("SELECT key, album_artist, title, remote_id FROM albums WHERE id = ?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let mut st = tx.prepare_cached("SELECT DISTINCT s.album_id FROM tracks t JOIN remote_songs s ON s.id = t.remote_id WHERE t.album_id = ?1")?;
    out.extend(st.query_map([id], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    let Some((key, artist, title, rid)) = row else { return Ok(()) };
    out.extend(rid);
    // The server albums its files are songs of.
    let mine: Vec<(i64, String)> = {
        let mut st = tx.prepare_cached(&format!("SELECT t.path, t.size FROM tracks t WHERE t.album_id = ?1 AND t.size > 0 AND {LOCAL_T}"))?;
        st.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.filter_map(|r| r.ok()).filter_map(|(p, s)| file_key(&p, s)).collect()
    };
    if !mine.is_empty() {
        let index = match files {
            Some(f) => f,
            None => files.insert(RemoteFiles::load(tx)?),
        };
        for k in &mine {
            out.extend(index.by_key.get(k).into_iter().flatten().cloned());
        }
    }
    if let Some(mb) = key.strip_prefix("mb:") {
        let mut st = tx.prepare_cached("SELECT id FROM remote_albums WHERE mb_album_id = ?1")?;
        out.extend(st.query_map([mb], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    let mut st = tx.prepare_cached("SELECT id FROM remote_albums WHERE match_key = ?1")?;
    out.extend(st.query_map([match_key(&artist, &title)], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(())
}

/// After a scan (and at the end of a sync): match again what the scan wrote (`remote_pending`)
/// with the server's catalog. A local file that appeared for a server-only track becomes one row
/// with it (the row with the history kept, the other's history moved to it); a local album that
/// appeared for a server album takes in its tracks. One query when there is nothing to do.
pub fn reconcile(conn: &mut Connection) -> rusqlite::Result<Reconciled> {
    let mut out = Reconciled::default();
    let pending: Vec<i64> = conn.prepare("SELECT album_id FROM remote_pending ORDER BY album_id")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if pending.is_empty() {
        return Ok(out);
    }
    if !connected(conn)? {
        conn.execute("DELETE FROM remote_pending", [])?;
        return Ok(out);
    }
    let mut unsettled = Unsettled::default();
    let mut done: HashSet<String> = HashSet::new();
    // The catalog does not change here: its files are indexed once.
    let mut files: Option<RemoteFiles> = None;
    for chunk in pending.chunks(ALBUMS_PER_TX) {
        crate::db::let_waiting_in();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut rids = Vec::new();
        for &id in chunk {
            candidates(&tx, id, &mut rids, &mut files)?;
        }
        rids.sort();
        rids.dedup();
        let mut w = Work::default();
        for rid in rids {
            if done.insert(rid.clone()) {
                place(&tx, &rid, &mut w)?;
                out.albums += 1;
            }
        }
        out.attached += w.attached;
        out.merged += w.merged;
        finish(&tx, &mut w)?;
        let mut del = tx.prepare_cached("DELETE FROM remote_pending WHERE album_id = ?1")?;
        for &id in chunk {
            del.execute([id])?;
        }
        drop(del);
        tx.commit()?;
        unsettled.take(&mut w);
    }
    unsettled.settle(conn, true)?;
    Ok(out)
}

/// A track whose file went away (a scan, a folder removed from the library): if the server has
/// it, its row stays as the server's track (its id, plays, favourite, playlist places,
/// album), written from the catalog as kept. `false`: the server does not have it (remove it as
/// before).
pub(crate) fn to_server_only(tx: &Connection, id: i64, touched: &mut Touched) -> rusqlite::Result<bool> {
    let Some((song, album)) = tx
        .prepare_cached("SELECT remote_id, album_id FROM tracks WHERE id = ?1 AND remote_id IS NOT NULL")?
        .query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .optional()?
    else {
        return Ok(false);
    };
    let Some(rid) = tx.prepare_cached("SELECT album_id FROM remote_songs WHERE id = ?1")?.query_row([&song], |r| r.get::<_, String>(0)).optional()? else {
        return Ok(false);
    };
    let Some(k) = load_album(tx, &rid)? else { return Ok(false) };
    let Some(ks) = k.songs.iter().find(|s| s.song.id == song) else { return Ok(false) };
    let mut w = Work::default();
    write_song(tx, Some(id), album, &k.album, ks, &mut w)?;
    touched.norms.extend(w.touched.norms);
    touched.artists.extend(w.touched.artists);
    touched.genres.extend(w.touched.genres);
    Ok(true)
}

// ---- the same server under another name ------------------------------------------

/// What the kept catalog holds: albums, songs.
pub fn kept(conn: &Connection) -> rusqlite::Result<(usize, usize)> {
    conn.query_row("SELECT (SELECT COUNT(*) FROM remote_albums), (SELECT COUNT(*) FROM remote_songs)", [], |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)))
}

/// How many of `ids` the kept catalog has as albums.
pub fn kept_albums_among<'a>(conn: &Connection, ids: impl IntoIterator<Item = &'a str>) -> rusqlite::Result<usize> {
    let mut st = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM remote_albums WHERE id = ?1)")?;
    let mut n = 0;
    for id in ids {
        n += usize::from(st.query_row([id], |r| r.get::<_, bool>(0))?);
    }
    Ok(n)
}

/// How many of `ids` the kept catalog has as songs.
pub fn kept_songs_among<'a>(conn: &Connection, ids: impl IntoIterator<Item = &'a str>) -> rusqlite::Result<usize> {
    let mut st = conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM remote_songs WHERE id = ?1)")?;
    let mut n = 0;
    for id in ids {
        n += usize::from(st.query_row([id], |r| r.get::<_, bool>(0))?);
    }
    Ok(n)
}

/// The server in the library is the one now signed in to, under another name (`identity`: its
/// other address, or another spelling of the user, or another user of it): the catalog and
/// everything of it stay, only the name changes. `other_user`: signed in as another user of
/// it, whose favourites on the server are not the ones agreed: every agreement is forgotten
/// (`remote_marks`), so the next pass takes the union of the two sides as at a first sync. Kept,
/// the old user's agreements would make the new user's differing stars look like changes made
/// on the server and unstar local favourites, and a local change made long ago would unstar the
/// new user's; the union removes nothing on either side. The plays not yet reported stay queued
/// (`remote_plays`) and go to the account signed in to now: they are this library's plays, the
/// earlier sign-in is gone, and dropping them would lose them.
pub fn rename(conn: &mut Connection, identity: &str, other_user: bool) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE remote_server SET identity = ?1 WHERE id = 1", [identity])?;
    if other_user {
        tx.execute("DELETE FROM remote_marks", [])?;
    }
    tx.commit()
}

// ---- disconnecting -------------------------------------------------------------

/// The server leaves the library: its server-only tracks go (with their plays), local tracks
/// and albums lose their `remote_id` and are otherwise as they were, the catalog is forgotten. A
/// batch at a time; an interrupted one is finished by the next call. Returns the tracks removed.
pub fn disconnect(conn: &mut Connection) -> rusqlite::Result<usize> {
    let mut removed = 0;
    let mut unsettled = Unsettled::default();
    loop {
        let ids: Vec<i64> = conn
            .prepare(&format!("SELECT t.id FROM tracks t WHERE t.remote_id IS NOT NULL AND NOT ({LOCAL_T}) LIMIT {BATCH_SONGS}"))?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ids.is_empty() {
            break;
        }
        crate::db::let_waiting_in();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut w = Work::default();
        for id in &ids {
            delete_track(&tx, *id, &mut w)?;
        }
        finish(&tx, &mut w)?;
        tx.commit()?;
        removed += ids.len();
        unsettled.take(&mut w);
        unsettled.settle(conn, false)?;
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE tracks SET remote_id = NULL WHERE remote_id IS NOT NULL", [])?;
    tx.execute("UPDATE albums SET remote_id = NULL, remote_art = NULL WHERE remote_id IS NOT NULL OR remote_art IS NOT NULL", [])?;
    tx.execute("DELETE FROM remote_songs", [])?;
    tx.execute("DELETE FROM remote_albums", [])?;
    tx.execute("DELETE FROM remote_pending", [])?;
    tx.execute("DELETE FROM remote_marks", [])?;
    tx.execute("DELETE FROM remote_plays", [])?;
    tx.execute("DELETE FROM remote_server", [])?;
    tx.commit()?;
    unsettled.settle(conn, true)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, disc: Option<u32>, track: Option<u32>, title: &str, duration: f64) -> RemoteSong {
        RemoteSong { id: id.into(), title: title.into(), disc, track, duration, ..Default::default() }
    }

    fn local(id: i64, disc: u32, track: Option<u32>, title: &str, duration: f64) -> LocalTrack {
        LocalTrack { id, disc, track, title: title.into(), duration, mb: None, file: None }
    }

    #[test]
    fn tracks_match_by_numbers_with_title_or_duration() {
        let l = [local(1, 1, Some(1), "Intro", 60.0), local(2, 1, Some(2), "Song", 200.0), local(3, 1, Some(3), "Café", 180.0)];
        let s = [song("a", None, Some(1), "intro", 61.5), song("b", Some(1), Some(2), "Song (Remastered)", 201.0), song("c", None, Some(3), "CAFE", 240.0)];
        let refs: Vec<&RemoteSong> = s.iter().collect();
        let m = match_tracks(&l, &refs);
        assert_eq!(m.pairs, vec![(0, 0), (1, 1), (2, 2)], "title, duration, folded title");
        assert_eq!(m.conflicts, 0);
        // The same number, nothing else agreeing: another track.
        let s = [song("x", None, Some(2), "Other", 300.0)];
        let refs: Vec<&RemoteSong> = s.iter().collect();
        let m = match_tracks(&l, &refs);
        assert!(m.pairs.is_empty());
        assert_eq!(m.conflicts, 1);
    }

    #[test]
    fn recording_ids_decide_when_both_have_one() {
        let mut l = [local(1, 1, Some(1), "A", 100.0), local(2, 1, Some(2), "B", 100.0)];
        l[0].mb = Some("r1".into());
        l[1].mb = Some("r2".into());
        let mut s = [song("a", None, Some(2), "A", 100.0), song("b", None, Some(1), "B", 100.0)];
        s[0].mb_recording_id = Some("R1".into());
        s[1].mb_recording_id = Some("r9".into());
        let refs: Vec<&RemoteSong> = s.iter().collect();
        let m = match_tracks(&l, &refs);
        assert_eq!(m.pairs, vec![(0, 0)], "by id across numbers; a different id never matches by number");
    }

    #[test]
    fn a_match_must_be_the_only_candidate_both_ways() {
        // Two local tracks at 1-1 (no disc numbers on a two-disc copy), both 200 s.
        let l = [local(1, 1, Some(1), "One", 200.0), local(2, 1, Some(1), "Uno", 200.5)];
        let s = [song("a", Some(1), Some(1), "Ein", 200.2)];
        let refs: Vec<&RemoteSong> = s.iter().collect();
        assert!(match_tracks(&l, &refs).pairs.is_empty());
    }

    #[test]
    fn paths_and_codecs() {
        assert!(is_remote(&song_path("abc")));
        assert_eq!(song_of_path(&song_path("x/y")), Some("x/y"));
        assert!(!is_remote(r"C:\Music\navidrome:x.flac"));
        assert!(!is_remote("/home/me/navidrome:x.flac"));
        assert_eq!(codec_of(Some("FLAC"), None, Some(16)), Codec::Flac);
        assert_eq!(codec_of(Some("m4a"), Some("audio/mp4"), Some(24)), Codec::Alac);
        assert_eq!(codec_of(Some("m4a"), Some("audio/mp4"), None), Codec::Aac);
        assert_eq!(codec_of(Some("ogg"), Some("audio/ogg; codecs=opus"), None), Codec::Opus);
        assert_eq!(codec_of(None, Some("audio/mpeg"), None), Codec::Mp3);
        assert_eq!(match_key("Various Artists", "Hits"), match_key("VA", " hits "));
    }
}
