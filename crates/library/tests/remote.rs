//! A server (Navidrome) beside the music folders, one library (`library::remote`): real files
//! (tiny WAVs with ID3 tags) scanned into a database on disk, and server catalogs as the app hands
//! them over. Server-only, local-only, merged and partly merged albums; the matches that must and
//! must not happen; syncs that are repeated, stopped, shrunk; scans before and after syncs; files
//! that go and come back; a folder removed, the server disconnected; a large catalog.

#[path = "support/scan_dump.rs"]
mod scan_dump;

use std::path::PathBuf;

use library::remote::{self, Catalog, RemoteAlbum, RemoteArtist, RemoteSong, SyncOptions, SyncReport};
use library::{ArtCache, ShelfQuery, query};
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag, TagType};
use rusqlite::Connection;

#[derive(Clone, Debug)]
struct Tr {
    title: String,
    disc: Option<u32>,
    no: Option<u32>,
    secs: u32,
}

fn tr(no: u32, title: &str, secs: u32) -> Tr {
    Tr { title: title.into(), disc: None, no: Some(no), secs }
}

#[derive(Clone, Debug)]
struct Al {
    artist: String,
    title: String,
    year: Option<String>,
    mb: Option<String>,
    genre: String,
    tracks: Vec<Tr>,
}

fn al(artist: &str, title: &str, year: Option<&str>, mb: Option<&str>, tracks: Vec<Tr>) -> Al {
    Al { artist: artist.into(), title: title.into(), year: year.map(Into::into), mb: mb.map(Into::into), genre: "Rock".into(), tracks }
}

/// `n` tracks named "<prefix> 1".. with lengths 100 + 7k s.
fn tracks(prefix: &str, n: u32) -> Vec<Tr> {
    (1..=n).map(|i| tr(i, &format!("{prefix} {i}"), 100 + 7 * i)).collect()
}

/// The server's copy of `a` as album `id`: every track, or `only` these numbers (1-based index).
fn server(id: &str, a: &Al, only: Option<&[usize]>) -> RemoteAlbum {
    let songs = a
        .tracks
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|o| o.contains(&(i + 1))))
        .map(|(_, t)| RemoteSong {
            id: format!("{id}-{}-{}", t.disc.unwrap_or(1), t.no.unwrap_or(0)),
            title: t.title.clone(),
            artist: a.artist.clone(),
            artists: vec![RemoteArtist { name: a.artist.clone(), ..Default::default() }],
            disc: t.disc,
            track: t.no,
            duration: f64::from(t.secs) + 0.4,
            genres: vec![a.genre.clone()],
            created: Some(1_600_000_000_000),
            suffix: Some("flac".into()),
            content_type: Some("audio/flac".into()),
            size: 1_000_000 + i64::from(t.secs),
            bitrate: Some(900),
            bit_depth: Some(16),
            sample_rate: Some(44_100),
            channels: Some(2),
            ..Default::default()
        })
        .collect();
    RemoteAlbum {
        id: id.into(),
        title: a.title.clone(),
        artist: a.artist.clone(),
        artists: vec![RemoteArtist { name: a.artist.clone(), ..Default::default() }],
        date: a.year.clone(),
        genres: vec![a.genre.clone()],
        label: Some("Server Label".into()),
        release_types: vec!["album".into()],
        mb_album_id: a.mb.clone(),
        created: Some(1_600_000_000_000 + id.len() as i64),
        cover_art: Some(format!("al-{id}")),
        songs,
        ..Default::default()
    }
}

fn catalog(albums: Vec<RemoteAlbum>) -> Catalog {
    Catalog { albums, complete: true, server_scan: Some("2026-11-01T00:00:00Z".into()) }
}

const SERVER: &str = "https://music.example/ (me)";

struct Lib {
    dir: PathBuf,
    conn: Connection,
    art: ArtCache,
    started: i64,
}

impl Lib {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mp-remote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        let conn = library::db::open(&dir.join("library.db")).unwrap();
        let art = ArtCache::new(dir.join("art"));
        Lib { dir, conn, art, started: library::db::now_ms() }
    }

    fn music(&self) -> PathBuf {
        self.dir.join("music")
    }

    /// Album `a` as files in `folder`: every track, or `only` these (1-based index).
    fn files(&self, folder: &str, a: &Al, only: Option<&[usize]>) {
        for (i, t) in a.tracks.iter().enumerate() {
            if only.is_some_and(|o| !o.contains(&(i + 1))) {
                continue;
            }
            let disc = t.disc.map(|d| format!("{d}-")).unwrap_or_default();
            self.wav(&format!("{folder}/{disc}{:02} {}.wav", t.no.unwrap_or(0), t.title), a, t);
        }
    }

    /// One short WAV (100 Hz, 8-bit mono: `secs` long in `secs * 100` bytes) with ID3 tags.
    fn wav(&self, rel: &str, a: &Al, t: &Tr) {
        let path = self.music().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rate = 100u32;
        let data = vec![0x80u8; (t.secs * rate) as usize];
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&8u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        std::fs::write(&path, w).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_text(ItemKey::AlbumArtist, a.artist.clone());
        tag.insert_text(ItemKey::TrackArtist, a.artist.clone());
        tag.insert_text(ItemKey::AlbumTitle, a.title.clone());
        tag.insert_text(ItemKey::TrackTitle, t.title.clone());
        tag.insert_text(ItemKey::Genre, "Local Genre".into());
        if let Some(n) = t.no {
            tag.set_track(n);
        }
        if let Some(d) = t.disc {
            tag.set_disk(d);
        }
        if let Some(y) = &a.year {
            tag.insert_text(ItemKey::RecordingDate, y.clone());
        }
        if let Some(mb) = &a.mb {
            tag.insert_text(ItemKey::MusicBrainzReleaseId, mb.clone());
        }
        tag.save_to_path(&path, WriteOptions::default()).unwrap();
    }

    fn scan(&mut self) -> library::scan::Report {
        let roots = vec![self.music()];
        let r = library::scan::scan(&mut self.conn, &self.art, &roots, &|_| {}).unwrap();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        r
    }

    fn sync(&mut self, albums: Vec<RemoteAlbum>) -> SyncReport {
        self.sync_with(&catalog(albums), &SyncOptions::default())
    }

    fn sync_with(&mut self, c: &Catalog, opts: &SyncOptions) -> SyncReport {
        remote::sync(&mut self.conn, SERVER, c, opts).unwrap_or_else(|e| panic!("{e}"))
    }

    fn reconcile(&mut self) -> remote::Reconciled {
        remote::reconcile(&mut self.conn).unwrap()
    }

    fn one(&self, sql: &str) -> i64 {
        self.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// Each album and its tracks, as a list shows them: album title, artist, where it is (a
    /// folder or the server), its server id; each track's numbers, title, whether it is a file
    /// and its song id.
    fn rows(&self) -> Vec<String> {
        let mut st = self
            .conn
            .prepare(
                "SELECT a.title, a.album_artist, CASE WHEN a.folder LIKE 'navidrome:%' THEN 'server' ELSE 'folder' END, COALESCE(a.remote_id, '-'),
                        COALESCE(t.disc_no, 1), COALESCE(t.track_no, 0), t.title, CASE WHEN t.path LIKE 'navidrome:%' THEN 'server' ELSE 'file' END,
                        COALESCE(t.remote_id, '-')
                 FROM tracks t JOIN albums a ON a.id = t.album_id ORDER BY a.title, a.album_artist, a.id, 5, 6, t.title",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok(format!(
                "{} / {} [{} {}] {}-{} {} ({} {})",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Albums: (title, artist, number of tracks, file tracks).
    fn albums(&self) -> Vec<(String, String, i64, i64)> {
        let mut st = self
            .conn
            .prepare(
                "SELECT a.title, a.album_artist, COUNT(t.id), COALESCE(SUM(t.path NOT LIKE 'navidrome:%'), 0) FROM albums a LEFT JOIN tracks t ON t.album_id = a.id
                 GROUP BY a.id ORDER BY a.title, a.album_artist, a.date",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
    }

    /// `dump`, with what differs between two libraries of the same music in two folders written
    /// at two moments taken out: the folder (`<music>`), the files' times, the folders' art
    /// fingerprints, the albums' dates.
    fn shape(&self) -> Vec<(&'static str, String)> {
        let music = self.music().to_string_lossy().into_owned();
        let forms = [music.clone(), music.to_lowercase(), music.replace('\\', "/"), music.to_lowercase().replace('\\', "/")];
        self.dump()
            .into_iter()
            .map(|(w, s)| {
                let s = s
                    .lines()
                    .map(|l| {
                        let mut l = l.to_owned();
                        for f in &forms {
                            l = l.replace(f.as_str(), "<music>");
                        }
                        let mut cols: Vec<String> = l.split(" | ").map(str::to_owned).collect();
                        let mask: &[usize] = match w {
                            "tracks" => &[7],
                            "albums" => &[13, 17, 18],
                            _ => &[],
                        };
                        for &i in mask {
                            if let Some(c) = cols.get_mut(i) {
                                *c = "*".into();
                            }
                        }
                        cols.join(" | ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (w, s)
            })
            .collect()
    }

    /// Everything a user sees and owns (`scan_dump`), plus the server's columns.
    fn dump(&self) -> Vec<(&'static str, String)> {
        let mut d = scan_dump::dump(&self.conn, self.started, 0);
        let q = |sql: &str| -> String {
            let mut st = self.conn.prepare(sql).unwrap();
            let n = st.column_count();
            st.query_map([], |r| {
                Ok((0..n)
                    .map(|i| match r.get::<_, rusqlite::types::Value>(i).unwrap() {
                        rusqlite::types::Value::Text(t) => t,
                        rusqlite::types::Value::Null => "-".into(),
                        v => format!("{v:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" | "))
            })
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
                .join("\n")
        };
        d.push(("track remote ids", q("SELECT path, remote_id FROM tracks ORDER BY path")));
        d.push(("album remote ids", q("SELECT key, remote_id, remote_art FROM albums ORDER BY key")));
        d.push(("mb track ids", q("SELECT t.path, g.mb_track_id FROM track_tags g JOIN tracks t ON t.id = g.track_id ORDER BY t.path")));
        d
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        // The database file closed first: Windows does not remove a file that is open.
        if let Ok(c) = Connection::open_in_memory() {
            drop(std::mem::replace(&mut self.conn, c));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn id_of(lib: &Lib, sql: &str) -> i64 {
    lib.one(sql)
}

/// A library of a server alone: its albums and tracks are rows like any other, filled by the
/// scanner's rules (artists, genres, label, dates, release type, sort), dated by the server, and
/// every page, search and smart playlist finds them.
#[test]
fn a_server_alone_is_a_library() {
    let mut lib = Lib::new("server-only");
    let mut jazz = server("j1", &al("Miles Example", "Blue Tuesday", Some("1959-08-17"), None, tracks("Blue", 3)), None);
    jazz.genres = vec!["Jazz".into()];
    for s in &mut jazz.songs {
        s.genres = vec!["Jazz".into(), "Modal".into()];
    }
    jazz.songs[1].artist = "Miles Example feat. Guest Horn".into();
    jazz.songs[1].artists.push(RemoteArtist { name: "Guest Horn".into(), ..Default::default() });
    jazz.artist_sort = Some("Example, Miles".into());
    jazz.created = Some(1_500_000_000_000);
    let mut various = server("v1", &al("Various Artists", "Hits Today", Some("2020"), Some("ABCD-1"), tracks("Hit", 2)), None);
    various.compilation = true;
    various.songs[0].artist = "Singer One".into();
    various.songs[0].artists = vec![RemoteArtist { name: "Singer One".into(), ..Default::default() }];
    various.songs[1].artist = "Singer Two".into();
    various.songs[1].artists = vec![RemoteArtist { name: "Singer Two".into(), ..Default::default() }];
    let r = lib.sync(vec![jazz, various]);
    assert_eq!((r.albums, r.songs, r.changed_albums, r.changed_songs, r.interrupted), (2, 5, 2, 5, false));
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 5, both: 0 });
    assert_eq!(
        lib.rows(),
        [
            "Blue Tuesday / Miles Example [server j1] 1-1 Blue 1 (server j1-1-1)",
            "Blue Tuesday / Miles Example [server j1] 1-2 Blue 2 (server j1-1-2)",
            "Blue Tuesday / Miles Example [server j1] 1-3 Blue 3 (server j1-1-3)",
            "Hits Today / Various Artists [server v1] 1-1 Hit 1 (server v1-1-1)",
            "Hits Today / Various Artists [server v1] 1-2 Hit 2 (server v1-1-2)",
        ]
    );
    // Album values as the scanner works them out.
    let jazz_id = id_of(&lib, "SELECT id FROM albums WHERE remote_id = 'j1'");
    let facts = query::album_facts(&lib.conn, jazz_id).unwrap().unwrap();
    assert_eq!(facts.key, "nd:j1");
    assert_eq!(facts.folder, "navidrome:album/j1");
    assert_eq!((facts.date.as_deref(), facts.label.as_deref(), facts.release_type.as_deref()), (Some("1959-08-17"), Some("Server Label"), Some("album")));
    assert_eq!((facts.added_at, facts.added_source.as_deref()), (1_500_000_000_000, Some("server")));
    assert_eq!(facts.genres, ["Jazz", "Modal"]);
    assert_eq!(facts.first_track, None, "no file to look in for art");
    assert_eq!(facts.album_artist_sort.as_deref(), Some("Example, Miles"));
    let various_id = id_of(&lib, "SELECT id FROM albums WHERE remote_id = 'v1'");
    let facts = query::album_facts(&lib.conn, various_id).unwrap().unwrap();
    assert_eq!((facts.key.as_str(), facts.compilation), ("mb:abcd-1", true), "keyed by its release, as a local copy would be");
    let a = query::album(&lib.conn, jazz_id).unwrap().unwrap();
    assert!(a.remote_only);
    assert_eq!((a.remote_id.as_deref(), a.remote_art.as_deref()), (Some("j1"), Some("al-j1")));
    // Tracks: streamable, no file.
    let t = query::track_by_remote(&lib.conn, "j1-1-2").unwrap().unwrap();
    assert!(t.is_remote());
    assert_eq!(t.artist, "Miles Example feat. Guest Horn");
    assert_eq!((t.audio.codec, t.audio.bit_depth, t.audio.sample_rate), (library::audio::Codec::Flac, Some(16), Some(44_100)));
    assert_eq!(remote::stream_info(&lib.conn, t.id).unwrap().unwrap(), remote::StreamInfo {
        song_id: "j1-1-2".into(),
        suffix: Some("flac".into()),
        content_type: Some("audio/flac".into()),
        size: 1_000_114,
        bitrate_kbps: Some(900),
    });
    assert_eq!(query::track_artists(&lib.conn, t.id).unwrap().iter().map(|a| (a.name.as_str(), a.featured)).collect::<Vec<_>>(), [
        ("Miles Example", false),
        ("Guest Horn", true)
    ]);
    // Pages.
    let titles = |v: Vec<library::Album>| v.into_iter().map(|a| a.title).collect::<Vec<_>>();
    assert_eq!(titles(query::shelf(&lib.conn, &ShelfQuery::RecentlyAdded, 10).unwrap()), ["Hits Today", "Blue Tuesday"], "by the server's dates");
    assert_eq!(titles(query::shelf(&lib.conn, &ShelfQuery::RecentlyReleased, 10).unwrap()), ["Hits Today", "Blue Tuesday"]);
    assert_eq!(titles(query::shelf(&lib.conn, &ShelfQuery::Genre { name: "jazz".into() }, 10).unwrap()), ["Blue Tuesday"]);
    assert_eq!(titles(query::shelf(&lib.conn, &ShelfQuery::Label { name: "server label".into() }, 10).unwrap()).len(), 2);
    assert_eq!(titles(query::all_albums(&lib.conn, query::AlbumSort::Artist, false).unwrap()), ["Blue Tuesday", "Hits Today"], "Example, Miles before Various");
    let s = query::search(&lib.conn, "guest horn", 10).unwrap();
    assert_eq!(s.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Guest Horn"]);
    let s = query::search(&lib.conn, "hit 2", 10).unwrap();
    assert_eq!(s.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Hit 2"]);
    let singer = query::find_artist(&lib.conn, "Singer One").unwrap().unwrap();
    let (own, appears) = query::artist_albums(&lib.conn, singer.id).unwrap();
    assert_eq!((own.len(), titles(appears)), (0, vec!["Hits Today".to_owned()]));
    let index: Vec<String> = query::album_artists(&lib.conn).unwrap().into_iter().map(|g| g.name).collect();
    assert_eq!(index, ["Miles Example", "Various Artists"]);
    // Smart playlists: genre, and a path rule (a server track has none).
    let smart = |rules: &str| {
        let s = library::smart::Smart::from_json(&format!(r#"{{"v":1,"match":"all","rules":[{rules}]}}"#));
        let mut ids = library::smart::evaluate_ids(&lib.conn, &s, None, library::smart::Ctx::new(2_000_000_000, 0)).unwrap().0;
        ids.sort_unstable();
        ids.len()
    };
    assert_eq!(smart(r#"{"field":"genre","op":"is","value":"Modal"}"#), 3);
    assert_eq!(smart(r#"{"field":"path","op":"contains","value":"navidrome"}"#), 0);
    assert_eq!(smart(r#"{"field":"format","op":"is","value":"FLAC"}"#), 5);
    // What has no file says so.
    assert_eq!(library::meta::read(std::path::Path::new(&t.path), library::meta::ReadOptions::TAGS).unwrap_err(), remote::NO_FILE);
    assert!(library::edit::support(std::path::Path::new(&t.path)).is_err());
    assert!(library::lyrics::load(std::path::Path::new(&t.path)).is_none());
    assert!(library::tags::read(std::path::Path::new(&t.path), false).is_err());
    let all = query::album_tracks(&lib.conn, jazz_id).unwrap();
    let m3u = library::m3u::export(&all, &lib.dir.join("x.m3u8"), false);
    assert_eq!((m3u.lines().count(), library::m3u::skipped(&all)), (1, 3), "a server track has no file to list");
    assert!(query::track_paths(&lib.conn).unwrap().is_empty());
    // A play and a favourite are the user's own, as for any track.
    query::record_play(&lib.conn, t.id).unwrap();
    query::toggle_favourite_track(&lib.conn, t.id).unwrap();
    assert_eq!(titles(query::shelf(&lib.conn, &ShelfQuery::RecentlyPlayed, 10).unwrap()), ["Blue Tuesday"]);
    let s = remote::server(&lib.conn).unwrap().unwrap();
    assert_eq!((s.identity.as_str(), s.server_scan.as_deref(), s.synced_at.is_some(), s.sync_started), (SERVER, Some("2026-11-01T00:00:00Z"), true, None));
}

/// A library of folders alone is untouched by an empty server, a sync that saw nothing, and a
/// reconcile with no server.
#[test]
fn folders_alone_are_untouched_by_an_empty_server() {
    let mut lib = Lib::new("local-only");
    lib.files("A/One", &al("Band", "One", Some("2001"), None, tracks("One", 3)), None);
    lib.files("B/Two", &al("Other", "Two", None, Some("mb-two"), tracks("Two", 2)), None);
    lib.scan();
    let before = lib.dump();
    assert_eq!(lib.reconcile(), remote::Reconciled::default());
    let r = lib.sync_with(&Catalog { albums: Vec::new(), complete: false, server_scan: None }, &SyncOptions::default());
    assert_eq!((r.changed_albums, r.removed_songs, r.held), (0, 0, None));
    let r = lib.sync(Vec::new());
    assert_eq!((r.changed_albums, r.removed_songs, r.held), (0, 0, None));
    scan_dump::assert_same(&before, &lib.dump(), "after an empty server");
    lib.scan();
    scan_dump::assert_same(&before, &lib.dump(), "a rescan");
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 5, server: 0, both: 0 });
    assert_eq!(remote::disconnect(&mut lib.conn).unwrap(), 0);
    scan_dump::assert_same(&before, &lib.dump(), "after disconnecting");
}

/// The same albums in both: nothing listed twice, local paths, tags and dates kept, the server's
/// ids on the rows; by release id and by artist and title alike.
#[test]
fn the_same_albums_in_both_are_listed_once() {
    let mut lib = Lib::new("merged");
    let one = al("Band", "One", Some("2001"), Some("mb-one"), tracks("One", 4));
    let two = al("Other Band", "Two", Some("1999"), None, tracks("Two", 3));
    lib.files("Band/One", &one, None);
    lib.files("Other/Two", &two, None);
    lib.scan();
    let before = lib.dump();
    let albums_before = lib.albums();
    let mut s1 = server("s1", &one, None);
    s1.title = "ONE".into(); // the server spells it otherwise: the file's spelling stays
    let r = lib.sync(vec![s1, server("s2", &two, None)]);
    assert_eq!((r.attached, r.merged), (7, 0));
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 0, both: 7 });
    assert_eq!(lib.albums(), albums_before, "no album twice");
    // Everything as the files say it; only the server's ids added.
    let after = lib.dump();
    for ((what, a), (_, b)) in before.iter().zip(&after) {
        if *what != "track remote ids" && *what != "album remote ids" {
            assert_eq!(a, b, "{what} unchanged");
        }
    }
    assert!(lib.rows().iter().all(|r| r.contains("(file ")), "{:#?}", lib.rows());
    let one_id = id_of(&lib, "SELECT id FROM albums WHERE key = 'mb:mb-one'");
    let a = query::album(&lib.conn, one_id).unwrap().unwrap();
    assert_eq!((a.title.as_str(), a.remote_id.as_deref(), a.remote_only), ("One", Some("s1"), false));
    assert_eq!(query::album_facts(&lib.conn, one_id).unwrap().unwrap().genres, ["Local Genre"], "the files' genre, not the server's");
    let t = query::track_by_remote(&lib.conn, "s2-1-2").unwrap().unwrap();
    assert!(!t.is_remote() && t.path.ends_with("02 Two 2.wav"));
}

/// A local copy of part of an album: one album, the files where there are files, the server's
/// tracks for the rest; the album's values from the files.
#[test]
fn part_of_an_album_on_disk_is_the_server_album() {
    let mut lib = Lib::new("partial");
    let full = al("Band", "Long Album", Some("2010"), None, tracks("Song", 6));
    lib.files("Band/Long Album", &full, Some(&[1, 2, 4]));
    lib.scan();
    let mut s = server("long", &full, None);
    s.label = Some("Other Label".into());
    lib.sync(vec![s.clone()]);
    // A play counted on the server changes what is kept, nothing that is shown.
    let changes = lib.conn.total_changes();
    s.songs[2].play_count = Some(12);
    s.play_count = Some(40);
    let r = lib.sync(vec![s]);
    assert_eq!((r.changed_albums, r.changed_songs), (1, 1));
    assert_eq!(lib.conn.total_changes() - changes, 4, "the two catalog entries and the server's sync times");
    assert_eq!(remote::song(&lib.conn, "long-1-3").unwrap().unwrap().play_count, Some(12), "kept for later, not applied");
    assert_eq!(query::play_count(&lib.conn, query::track_by_remote(&lib.conn, "long-1-3").unwrap().unwrap().id).unwrap(), 0);
    assert_eq!(
        lib.rows(),
        [
            "Long Album / Band [folder long] 1-1 Song 1 (file long-1-1)",
            "Long Album / Band [folder long] 1-2 Song 2 (file long-1-2)",
            "Long Album / Band [folder long] 1-3 Song 3 (server long-1-3)",
            "Long Album / Band [folder long] 1-4 Song 4 (file long-1-4)",
            "Long Album / Band [folder long] 1-5 Song 5 (server long-1-5)",
            "Long Album / Band [folder long] 1-6 Song 6 (server long-1-6)",
        ]
    );
    let id = id_of(&lib, "SELECT id FROM albums");
    let f = query::album_facts(&lib.conn, id).unwrap().unwrap();
    assert_eq!((f.label, f.genres), (None, vec!["Local Genre".to_owned()]), "the files' values (no label tagged), not the server's");
    assert!(!f.folder.starts_with("navidrome:"));
    assert_eq!(query::album_tracks(&lib.conn, id).unwrap().len(), 6);
}

/// An album the server renames: its server-only tracks and the album follow; a merged album
/// keeps what its files say. A merged album the server renames is no longer the files' album by
/// its name: it goes back to being an album of its own.
#[test]
fn a_server_change_reaches_its_own_tracks_only() {
    let mut lib = Lib::new("server-change");
    let mine = al("Band", "Mine", Some("2010"), None, tracks("M", 3));
    lib.files("Band/Mine", &mine, Some(&[1]));
    lib.scan();
    let theirs = al("Them", "Old Title", Some("1990"), None, tracks("T", 2));
    let mut s_mine = server("mine", &mine, None);
    let mut s_theirs = server("theirs", &theirs, None);
    lib.sync(vec![s_mine.clone(), s_theirs.clone()]);
    s_theirs.title = "New Title".into();
    s_theirs.songs[0].title = "T one".into();
    s_mine.label = Some("New Label".into());
    s_mine.songs[0].title = "Server's title".into();
    s_mine.songs[1].title = "M two".into();
    lib.sync(vec![s_mine.clone(), s_theirs]);
    assert_eq!(
        lib.rows(),
        [
            "Mine / Band [folder mine] 1-1 M 1 (file mine-1-1)",
            "Mine / Band [folder mine] 1-2 M two (server mine-1-2)",
            "Mine / Band [folder mine] 1-3 M 3 (server mine-1-3)",
            "New Title / Them [server theirs] 1-1 T one (server theirs-1-1)",
            "New Title / Them [server theirs] 1-2 T 2 (server theirs-1-2)",
        ]
    );
    assert_eq!(lib.one("SELECT COUNT(*) FROM track_tags WHERE album_title = 'Old Title'"), 0);
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums WHERE label IS NOT NULL"), 1, "the server's label on its own album only");
    s_mine.title = "Mine (Remaster)".into();
    lib.sync(vec![s_mine]);
    assert_eq!(
        lib.rows()[..4],
        [
            "Mine / Band [folder -] 1-1 M 1 (file -)",
            "Mine (Remaster) / Band [server mine] 1-1 Server's title (server mine-1-1)",
            "Mine (Remaster) / Band [server mine] 1-2 M two (server mine-1-2)",
            "Mine (Remaster) / Band [server mine] 1-3 M 3 (server mine-1-3)",
        ]
    );
}

/// The matches that must not happen (two rows is the safe failure), and the ones that must.
#[test]
fn uncertain_albums_stay_apart() {
    let mut lib = Lib::new("uncertain");
    // Standard edition on disk; the server has the deluxe edition: another title, or the same
    // title with other release ids, or the same title of another year.
    let standard = al("Singer", "Album", Some("2001"), None, tracks("Song", 10));
    let mut deluxe_tracks = tracks("Song", 10);
    deluxe_tracks.extend((11..=14).map(|i| tr(i, &format!("Bonus {i}"), 300)));
    lib.files("Singer/Album", &standard, None);
    let deluxe_titled = al("Singer", "Album (Deluxe Edition)", Some("2001"), None, deluxe_tracks.clone());
    let deluxe_reissue = al("Singer", "Album", Some("2011"), None, deluxe_tracks.clone());
    // A two-disc set, on disk and on the server.
    let mut two_disc = al("Orchestra", "Complete Works", Some("2005"), None, Vec::new());
    for d in 1..=2 {
        for n in 1..=3 {
            two_disc.tracks.push(Tr { title: format!("Part {d}.{n}"), disc: Some(d), no: Some(n), secs: 200 + d * 10 + n });
        }
    }
    lib.files("Orchestra/Complete Works", &two_disc, None);
    // "Greatest Hits" by two artists: only one on disk.
    let hits_a = al("Artist A", "Greatest Hits", None, None, tracks("A hit", 4));
    let hits_b = al("Artist B", "Greatest Hits", None, None, tracks("B hit", 4));
    lib.files("A/Greatest Hits", &hits_a, None);
    // Two self-titled albums of one band, years 1994 and 2001, on disk; and on the server.
    let blue = al("Weez", "Weez", Some("1994"), None, tracks("Blue", 5));
    let green = al("Weez", "Weez", Some("2001"), None, tracks("Green", 5));
    lib.files("Weez/Weez (1994)", &blue, None);
    lib.files("Weez/Weez (2001)", &green, None);
    // Two undated copies of one title by one artist with different songs: neither can be told.
    let x1 = al("Twin", "Same", None, None, tracks("First", 3));
    let x2 = al("Twin", "Same", None, None, tracks("Second", 3));
    lib.files("Twin/Same 1", &x1, None);
    lib.files("Twin/Same 2", &x2, None);
    lib.scan();
    let files_before = lib.one("SELECT COUNT(*) FROM tracks");
    let albums_before = lib.one("SELECT COUNT(*) FROM albums");
    lib.sync(vec![
        server("deluxe-titled", &deluxe_titled, None),
        server("deluxe-reissue", &deluxe_reissue, None),
        server("works", &two_disc, None),
        server("hits-a", &hits_a, None),
        server("hits-b", &hits_b, None),
        server("weez-2001", &green, None),
        server("weez-1994", &blue, None),
        server("same", &al("Twin", "Same", None, None, tracks("Third", 3)), None),
    ]);
    let held = |rid: &str| -> (i64, i64) {
        lib.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(t.path NOT LIKE 'navidrome:%'), 0) FROM tracks t JOIN albums a ON a.id = t.album_id WHERE a.remote_id = ?1",
                [rid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(held("deluxe-titled"), (14, 0), "another title: an album of its own");
    assert_eq!(held("deluxe-reissue"), (14, 0), "another year: an album of its own");
    assert_eq!(held("works"), (6, 6), "both discs, every track matched by disc and number");
    assert_eq!(held("hits-a"), (4, 4));
    assert_eq!(held("hits-b"), (4, 0), "the other artist's Greatest Hits stays apart");
    assert_eq!(held("weez-1994"), (5, 5), "the self-titled album of its year");
    assert_eq!(held("weez-2001"), (5, 5));
    assert_eq!(held("same"), (3, 0), "either undated copy could be it: neither is taken");
    assert_eq!(lib.one("SELECT COUNT(*) FROM tracks WHERE path NOT LIKE 'navidrome:%'"), files_before, "no file's row lost or doubled");
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums"), albums_before + 4);
    let blue_files: i64 = lib.one(
        "SELECT COUNT(*) FROM tracks t JOIN albums a ON a.id = t.album_id WHERE a.remote_id = 'weez-1994' AND t.title LIKE 'Blue%' AND t.path NOT LIKE 'navidrome:%'",
    );
    assert_eq!(blue_files, 5);
    // Same title, other release ids: apart even with the same songs.
    let mut lib2 = Lib::new("uncertain-mb");
    let local = al("Singer", "Album", None, Some("release-a"), tracks("Song", 5));
    lib2.files("Singer/Album", &local, None);
    lib2.scan();
    let mut other = local.clone();
    other.mb = Some("release-b".into());
    lib2.sync(vec![server("other-release", &other, None)]);
    assert_eq!(lib2.one("SELECT COUNT(*) FROM albums"), 2);
    assert_eq!(lib2.one("SELECT COUNT(*) FROM tracks WHERE remote_id IS NOT NULL AND path NOT LIKE 'navidrome:%'"), 0);
    // The same songs on disk without release ids: the server's release joins them by name.
    let mut lib3 = Lib::new("uncertain-name");
    let mut untagged = local.clone();
    untagged.mb = None;
    lib3.files("Singer/Album", &untagged, None);
    lib3.scan();
    lib3.sync(vec![server("tagged", &local, None)]);
    assert_eq!(lib3.one("SELECT COUNT(*) FROM albums"), 1);
    assert_eq!(remote::counts(&lib3.conn).unwrap().both, 5);
}

/// Recording ids, read from the files from now on (`track_tags.mb_track_id`), pair tracks
/// whatever their numbers say.
#[test]
fn recording_ids_pair_tracks_across_numbers() {
    let mut lib = Lib::new("recording-ids");
    let a = al("Band", "Ids", Some("2001"), None, vec![tr(1, "Alpha", 100), tr(2, "Beta", 150), tr(3, "Gamma", 200)]);
    for (t, id) in a.tracks.iter().zip(["rec-a", "rec-b", "rec-c"]) {
        let rel = format!("Band/Ids/{:02} {}.wav", t.no.unwrap(), t.title);
        lib.wav(&rel, &a, t);
        let path = lib.music().join(&rel);
        let mut tagged = lofty::read_from_path(&path).unwrap();
        tagged.primary_tag_mut().unwrap().insert_text(ItemKey::MusicBrainzRecordingId, id.into());
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();
    }
    lib.scan();
    assert_eq!(lib.one("SELECT COUNT(*) FROM track_tags WHERE mb_track_id LIKE 'rec-%'"), 3, "read with the tags");
    // The server numbers them the other way round (another track list of the same recordings).
    let mut s = server("ids", &a, None);
    for (song, (no, id)) in s.songs.iter_mut().zip([(3, "REC-A"), (2, "rec-b"), (1, "rec-c")]) {
        song.track = Some(no);
        song.mb_recording_id = Some(id.into());
    }
    let r = lib.sync(vec![s]);
    assert_eq!(r.attached, 3);
    assert_eq!(query::track_by_remote(&lib.conn, "ids-1-1").unwrap().unwrap().title, "Alpha");
    assert_eq!(query::track_by_remote(&lib.conn, "ids-1-3").unwrap().unwrap().title, "Gamma");
    assert_eq!(remote::counts(&lib.conn).unwrap().both, 3);
}

/// Running a sync twice changes nothing; one stopped after every batch and run again ends as one
/// that was not stopped.
#[test]
fn a_sync_is_idempotent_and_picks_up_where_it_stopped() {
    let build = |lib: &Lib| {
        for i in 0..6 {
            lib.files(&format!("L/{i}"), &al(&format!("Artist {i}"), &format!("Album {i}"), Some("2000"), None, tracks("T", 4)), Some(&[1, 3]));
        }
    };
    let albums: Vec<RemoteAlbum> = (0..12)
        .map(|i| server(&format!("r{i}"), &al(&format!("Artist {}", i % 6), &format!("Album {i}"), Some("2000"), None, tracks("T", 4)), None))
        .collect();
    let opts = SyncOptions { batch_songs: Some(8), ..Default::default() };
    let mut whole = Lib::new("whole");
    build(&whole);
    whole.scan();
    let first = whole.sync_with(&catalog(albums.clone()), &opts);
    assert!(first.batches >= 6, "{first:?}");
    let want = whole.shape();
    let changes = whole.conn.total_changes();
    let again = whole.sync_with(&catalog(albums.clone()), &opts);
    assert_eq!((again.changed_albums, again.changed_songs, again.attached, again.merged), (0, 0, 0, 0));
    assert!(whole.conn.total_changes() - changes <= 2, "only the server's sync times are written: {}", whole.conn.total_changes() - changes);
    scan_dump::assert_same(&want, &whole.shape(), "synced twice");

    let mut stopped = Lib::new("stopped");
    stopped.started = whole.started;
    build(&stopped);
    stopped.scan();
    let mut runs = 0;
    loop {
        runs += 1;
        let r = stopped.sync_with(&catalog(albums.clone()), &SyncOptions { stop_after: Some(1), ..opts.clone() });
        if !r.interrupted {
            break;
        }
        assert!(remote::server(&stopped.conn).unwrap().unwrap().sync_started.is_some());
        assert!(runs < 50);
    }
    assert!(runs > 3);
    scan_dump::assert_same(&want, &stopped.shape(), "stopped after every batch");
    assert_eq!(remote::server(&stopped.conn).unwrap().unwrap().sync_started, None);
}

/// A song the server lists under two albums is kept under the first that lists it: a second
/// sync of the same catalog writes nothing (under each in turn, it was written over and placed
/// again at every sync).
#[test]
fn a_song_listed_under_two_albums_stays_with_the_first() {
    let mut lib = Lib::new("twice-listed");
    let a = server("r1", &al("Ann", "First", Some("2000"), None, tracks("T", 3)), None);
    let mut b = server("r2", &al("Ann", "Second", Some("2001"), None, tracks("U", 2)), None);
    b.songs.push(a.songs[0].clone());
    let albums = vec![a.clone(), b];
    lib.sync(albums.clone());
    let holder: String = lib.conn.query_row("SELECT album_id FROM remote_songs WHERE id = ?1", [&a.songs[0].id], |r| r.get(0)).unwrap();
    assert_eq!(holder, "r1");
    assert_eq!(lib.one("SELECT COUNT(*) FROM tracks"), 5, "the song once");
    let changes = lib.conn.total_changes();
    let again = lib.sync(albums);
    assert_eq!((again.changed_albums, again.changed_songs), (0, 0), "{again:?}");
    assert!(lib.conn.total_changes() - changes <= 2, "only the sync times: {}", lib.conn.total_changes() - changes);
}

/// What the server no longer lists: a server-only track goes with its plays, a file's row only
/// loses its id; an empty or much smaller listing removes nothing unless confirmed, and an
/// incomplete one never removes.
#[test]
fn server_removals_and_the_shrink_guard() {
    let mut lib = Lib::new("removals");
    let a = al("Band", "Kept", Some("2001"), None, tracks("K", 3));
    lib.files("Band/Kept", &a, Some(&[1]));
    lib.scan();
    let mut big: Vec<RemoteAlbum> = vec![server("kept", &a, None)];
    big.extend((0..10).map(|i| server(&format!("x{i}"), &al("Many", &format!("M{i}"), None, None, tracks("M", 3)), None)));
    lib.sync(big.clone());
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 32, both: 1 });
    let gone = query::track_by_remote(&lib.conn, "kept-1-2").unwrap().unwrap();
    query::record_play(&lib.conn, gone.id).unwrap();
    // One song and one album gone; the file's song too.
    let mut fewer = big.clone();
    fewer[0].songs.retain(|s| s.id == "kept-1-3");
    fewer.pop();
    let r = lib.sync(fewer.clone());
    assert_eq!((r.removed_songs, r.removed_albums, r.held), (5, 1, None));
    assert!(query::track(&lib.conn, gone.id).unwrap().is_none());
    assert_eq!(lib.one(&format!("SELECT COUNT(*) FROM plays WHERE track_id = {}", gone.id)), 0, "its plays go with it, as a deleted file's");
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 1, server: 28, both: 0 }, "the file stays, without the server's id");
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums WHERE title = 'M9'"), 0);
    // Incomplete: nothing removed.
    let r = lib.sync_with(&Catalog { albums: fewer[..2].to_vec(), complete: false, server_scan: None }, &SyncOptions::default());
    assert_eq!(r.removed_songs, 0);
    assert_eq!(remote::counts(&lib.conn).unwrap().server, 28);
    // Empty and complete: held.
    let before = lib.dump();
    let r = lib.sync(Vec::new());
    assert_eq!(r.held, Some(remote::Held { missing: 28, known: 28 }));
    assert_eq!(r.removed_songs, 0);
    scan_dump::assert_same(&before, &lib.dump(), "an empty listing removes nothing");
    assert!(remote::server(&lib.conn).unwrap().unwrap().synced_at.is_some());
    // Confirmed: removed.
    let r = lib.sync_with(&catalog(Vec::new()), &SyncOptions { confirm_removal: true, ..Default::default() });
    assert_eq!((r.removed_songs, r.held), (28, None));
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 1, server: 0, both: 0 });
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums"), 1);
}

/// Files first or the server first: the same library.
#[test]
fn scan_then_sync_and_sync_then_scan_agree() {
    let both = al("Band", "Both", Some("2001"), Some("mb-both"), tracks("Both", 4));
    let part = al("Band", "Part", Some("2003"), None, tracks("Part", 5));
    let named = al("Duo", "Named", None, None, tracks("Named", 3));
    let server_only = al("Far", "Away", Some("1990"), None, tracks("Away", 2));
    let local_only = al("Near", "Home", None, None, tracks("Home", 2));
    let mut named_server = server("named", &named, None);
    named_server.mb_album_id = Some("mb-named".into());
    let cat = vec![server("both", &both, None), server("part", &part, None), named_server, server("away", &server_only, None)];
    let files = |lib: &Lib| {
        lib.files("Band/Both", &both, None);
        lib.files("Band/Part", &part, Some(&[2, 3]));
        lib.files("Duo/Named", &named, None);
        lib.files("Near/Home", &local_only, None);
    };
    let mut a = Lib::new("order-files-first");
    files(&a);
    a.scan();
    a.sync(cat.clone());
    a.reconcile();
    let mut b = Lib::new("order-server-first");
    b.started = a.started;
    b.sync(cat.clone());
    files(&b);
    b.scan();
    let r = b.reconcile();
    assert!(r.merged + r.attached > 0, "{r:?}");
    assert_eq!(b.reconcile(), remote::Reconciled::default(), "nothing left to do");
    scan_dump::assert_same(&a.shape(), &b.shape(), "files first vs server first");
    assert_eq!(a.rows(), b.rows());
    assert_eq!(
        a.albums(),
        [
            ("Away".to_owned(), "Far".to_owned(), 2, 0),
            ("Both".into(), "Band".into(), 4, 4),
            ("Home".into(), "Near".into(), 2, 2),
            ("Named".into(), "Duo".into(), 3, 3),
            ("Part".into(), "Band".into(), 5, 2),
        ]
    );
    // Dated alike: the files' albums by their files (a first import), the server's by the server.
    let sources = |l: &Lib| -> Vec<(String, Option<String>)> {
        let mut st = l.conn.prepare("SELECT title, added_source FROM albums ORDER BY title").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(sources(&a), sources(&b));
    assert_eq!(sources(&a)[0], ("Away".to_owned(), Some("server".to_owned())));
    // Every page reads the mixed library alike: server albums and tracks beside the files'.
    for lib in [&a, &b] {
        assert_eq!(query::shelf(&lib.conn, &ShelfQuery::RecentlyAdded, 10).unwrap().len(), 5);
        assert_eq!(query::shelf(&lib.conn, &ShelfQuery::Genre { name: "rock".into() }, 10).unwrap().iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Away"]);
        assert_eq!(query::search(&lib.conn, "away 2", 10).unwrap().tracks.len(), 1);
        let rules = library::smart::Smart::from_json(r#"{"v":1,"match":"all","rules":[{"field":"genre","op":"is","value":"Rock"}]}"#);
        let ids = library::smart::evaluate_ids(&lib.conn, &rules, None, library::smart::Ctx::new(2_000_000_000, 0)).unwrap().0;
        assert_eq!(ids.len(), 5, "the server's own tracks of Away and Part (the files say Local Genre)");
        let band = query::find_artist(&lib.conn, "Band").unwrap().unwrap();
        let (own, _) = query::artist_albums(&lib.conn, band.id).unwrap();
        assert_eq!(own.iter().map(|a| (a.title.as_str(), a.remote_only)).collect::<Vec<_>>(), [("Part", false), ("Both", false)], "newest first");
        let far = query::find_artist(&lib.conn, "Far").unwrap().unwrap();
        assert_eq!(query::artist_top_tracks(&lib.conn, far.id, 10).unwrap().len(), 2);
    }
}

/// A file the server also has goes away and comes back: one row all along, with its id, plays,
/// favourite, playlist place and queue place.
#[test]
fn a_file_removed_and_restored_keeps_its_row() {
    let mut lib = Lib::new("restore");
    let a = al("Band", "Album", Some("2001"), None, tracks("Song", 3));
    lib.files("Band/Album", &a, None);
    lib.scan();
    lib.sync(vec![server("al", &a, None)]);
    let t = query::track_by_remote(&lib.conn, "al-1-2").unwrap().unwrap();
    let path = t.path.clone();
    query::record_play(&lib.conn, t.id).unwrap();
    query::record_play(&lib.conn, t.id).unwrap();
    query::toggle_favourite_track(&lib.conn, t.id).unwrap();
    let other = query::track_by_remote(&lib.conn, "al-1-1").unwrap().unwrap().id;
    let pl = query::create_playlist_with(&lib.conn, "Mix", &[other, t.id, other]).unwrap();
    library::session::save_queue(&lib.conn, &[(t.id, None)], Some(0), 12.0).unwrap();
    let album = t.album_id;
    let history = |lib: &Lib, id: i64| {
        (
            query::play_count(&lib.conn, id).unwrap(),
            query::is_favourite_track(&lib.conn, id).unwrap(),
            query::playlist_tracks(&lib.conn, pl).unwrap().iter().map(|t| t.id).collect::<Vec<_>>(),
            library::session::load_queue(&lib.conn).unwrap().items.iter().map(|i| i.track.id).collect::<Vec<_>>(),
        )
    };
    let want = history(&lib, t.id);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let r = lib.scan();
    assert_eq!(r.removed, 1);
    let now = query::track(&lib.conn, t.id).unwrap().expect("the row stays");
    assert!(now.is_remote(), "{}", now.path);
    assert_eq!((now.album_id, now.remote_id.as_deref(), now.title.as_str()), (album, Some("al-1-2"), "Song 2"));
    assert_eq!(history(&lib, t.id), want);
    assert_eq!(lib.reconcile().merged, 0);
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 1, both: 2 });
    // Back again.
    std::fs::write(&path, bytes).unwrap();
    lib.scan();
    let r = lib.reconcile();
    assert_eq!(r.merged, 1);
    let back = query::track(&lib.conn, t.id).unwrap().expect("the same row");
    assert_eq!((back.path.as_str(), back.album_id, back.remote_id.as_deref()), (path.as_str(), album, Some("al-1-2")));
    assert_eq!(history(&lib, t.id), want);
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 0, both: 3 });
    assert_eq!(lib.one("SELECT COUNT(*) FROM tracks"), 3);
    // A track only the server had, played, then copied to disk: its history comes with it.
    let mut lib2 = Lib::new("restore-new");
    lib2.files("Band/Album", &a, Some(&[1, 2]));
    lib2.scan();
    lib2.sync(vec![server("al", &a, None)]);
    let s = query::track_by_remote(&lib2.conn, "al-1-3").unwrap().unwrap();
    assert!(s.is_remote());
    query::record_play(&lib2.conn, s.id).unwrap();
    lib2.files("Band/Album", &a, Some(&[3]));
    lib2.scan();
    lib2.reconcile();
    let now = query::track(&lib2.conn, s.id).unwrap().expect("the row with the history stays");
    assert!(!now.is_remote());
    assert_eq!(query::play_count(&lib2.conn, s.id).unwrap(), 1);
    assert_eq!(lib2.one("SELECT COUNT(*) FROM tracks"), 3);
}

/// A folder removed from the library: its files that the server also has stay as the server's;
/// the rest go; the server's own tracks are untouched. Disconnecting: the server's tracks go, the
/// files stay as they were before the server came.
#[test]
fn removing_a_folder_and_disconnecting() {
    let mut lib = Lib::new("forget");
    let a = al("Band", "Shared", Some("2001"), None, tracks("S", 3));
    let b = al("Band", "Mine", Some("2002"), None, tracks("M", 2));
    lib.files("Band/Shared", &a, None);
    lib.files("Band/Mine", &b, None);
    lib.scan();
    let before_server = lib.dump();
    lib.sync(vec![server("shared", &a, None), server("theirs", &al("Them", "Theirs", None, None, tracks("X", 2)), None)]);
    let t = query::track_by_remote(&lib.conn, "shared-1-1").unwrap().unwrap();
    query::record_play(&lib.conn, t.id).unwrap();
    let after_sync = lib.dump();
    let music = lib.music();
    library::scan::forget_root(&mut lib.conn, &music, &[]).unwrap();
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 5, both: 0 });
    let kept = query::track(&lib.conn, t.id).unwrap().unwrap();
    assert!(kept.is_remote());
    assert_eq!(query::play_count(&lib.conn, t.id).unwrap(), 1);
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums WHERE title = 'Mine'"), 0);
    // The folder back: the files rejoin their rows.
    lib.scan();
    lib.reconcile();
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 2, server: 2, both: 3 });
    assert_eq!(query::track(&lib.conn, t.id).unwrap().unwrap().path, t.path);
    let strip_dates = |d: Vec<(&'static str, String)>| -> Vec<(&'static str, String)> { d.into_iter().filter(|(w, _)| *w != "albums" && *w != "tracks").collect() };
    scan_dump::assert_same(&strip_dates(after_sync), &strip_dates(lib.dump()), "the folder back");
    // Disconnected: as before the server, plus the play.
    let removed = remote::disconnect(&mut lib.conn).unwrap();
    assert_eq!(removed, 2);
    assert!(!remote::connected(&lib.conn).unwrap());
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 5, server: 0, both: 0 });
    let mut after = lib.dump();
    for (w, s) in &mut after {
        if *w == "plays" {
            *s = before_server.iter().find(|(x, _)| x == w).unwrap().1.clone();
        }
    }
    let strip = |d: Vec<(&'static str, String)>| -> Vec<(&'static str, String)> { d.into_iter().filter(|(w, _)| *w != "albums" && *w != "tracks").collect() };
    scan_dump::assert_same(&strip(before_server.clone()), &strip(after), "disconnected");
    assert_eq!(lib.albums(), [("Mine".to_owned(), "Band".to_owned(), 2, 2), ("Shared".into(), "Band".into(), 3, 3)]);
    for t in ["remote_server", "remote_albums", "remote_songs", "remote_pending"] {
        assert_eq!(lib.one(&format!("SELECT COUNT(*) FROM {t}")), 0, "{t}");
    }
    // Another server can now be connected; one can not while the first is.
    lib.sync(vec![server("theirs", &al("Them", "Theirs", None, None, tracks("X", 2)), None)]);
    let e = remote::sync(&mut lib.conn, "https://other/ (me)", &catalog(Vec::new()), &SyncOptions::default()).unwrap_err();
    assert!(matches!(e, remote::SyncError::OtherServer { .. }), "{e}");
}

/// 50,000 server tracks (5,000 albums, a few hundred artists, a local library of 1,000 files
/// merged in): the sync writes in bounded transactions, in reasonable time; a second one writes
/// nothing.
#[test]
fn a_large_catalog_syncs_in_bounded_transactions() {
    let mut lib = Lib::new("large");
    // A local library to match against: 100 of the albums on disk (10 tracks each).
    let album = |i: usize| -> Al {
        let mut a = al(&format!("Artist {}", i % 400), &format!("Record {i}"), Some(&format!("{}", 1960 + i % 60)), None, tracks(&format!("Track {i}"), 10));
        a.genre = ["Rock", "Jazz", "Pop", "Folk", "Electronic"][i % 5].into();
        a
    };
    for i in (0..5000).step_by(50) {
        lib.files(&format!("A/{i}"), &album(i), None);
    }
    lib.scan();
    let albums: Vec<RemoteAlbum> = (0..5000).map(|i| server(&format!("al{i:05}"), &album(i), None)).collect();
    let t0 = std::time::Instant::now();
    let r = lib.sync(albums.clone());
    let first = t0.elapsed();
    assert_eq!((r.songs, r.attached, r.interrupted), (50_000, 1_000, false));
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 49_000, both: 1_000 });
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums"), 5000);
    let t1 = std::time::Instant::now();
    let again = lib.sync(albums);
    let second = t1.elapsed();
    assert_eq!((again.changed_albums, again.changed_songs), (0, 0));
    eprintln!(
        "50,000-track catalog: first sync {:.2} s in {} transactions, the longest {:.0} ms; second sync {:.2} s, longest {:.0} ms",
        first.as_secs_f64(),
        r.batches,
        r.longest_write * 1000.0,
        second.as_secs_f64(),
        again.longest_write * 1000.0
    );
    assert!(r.batches >= 90, "{} transactions", r.batches);
    // Generous for a debug build on a busy machine; a release build is several times faster.
    assert!(r.longest_write < 2.0, "the longest transaction held the database {:.2} s", r.longest_write);
    assert!(first.as_secs_f64() < 240.0);
    // Pages read the mixed library.
    assert_eq!(query::shelf(&lib.conn, &ShelfQuery::Genre { name: "folk".into() }, 2000).unwrap().len(), 1000);
    assert_eq!(query::search(&lib.conn, "record 4321", 10).unwrap().albums.len(), 1);
}

/// The sizes of the files under `rel` (`<music>/<rel>/...`), by file name.
fn sizes(lib: &Lib, rel: &str) -> std::collections::HashMap<String, i64> {
    std::fs::read_dir(lib.music().join(rel))
        .unwrap()
        .map(Result::unwrap)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap().len() as i64))
        .collect()
}

/// The server's copy of album `a` as the same files: each song's path on the server (`server_dir`,
/// under the server's own music folder) and its size as the local file's.
fn served(id: &str, a: &Al, server_dir: &str, files: &std::collections::HashMap<String, i64>) -> RemoteAlbum {
    let mut s = server(id, a, None);
    for (song, t) in s.songs.iter_mut().zip(&a.tracks) {
        let name = format!("{:02} {}.wav", t.no.unwrap_or(0), t.title);
        song.size = files[&name];
        song.path = Some(format!("{server_dir}/{name}"));
    }
    s
}

/// The same files on the server and on disk are one track each, and one album, though the server's
/// tags say otherwise (another title, artist, year and track titles): the path's last two
/// components (case and separators aside) and the size decide, before any tag.
#[test]
fn the_same_files_are_one_album_whatever_the_tags_say() {
    let mut lib = Lib::new("same-files");
    let a = al("Band", "Album", Some("2001"), None, tracks("Song", 4));
    lib.files("Band/Album", &a, None);
    lib.scan();
    let files = sizes(&lib, "Band/Album");
    let mut s = served("files", &a, "Music Library/BAND/album", &files);
    s.title = "Album (2011 Remaster)".into();
    s.artist = "The Band".into();
    s.artists = vec![RemoteArtist { name: "The Band".into(), ..Default::default() }];
    s.date = Some("2011".into());
    for (i, song) in s.songs.iter_mut().enumerate() {
        song.title = format!("Remastered {i}");
        song.path = song.path.take().map(|p| p.to_uppercase().replace('/', "\\"));
    }
    // A song elsewhere on the server with the name of a local file but another size: not that file.
    let mut other = server("other", &al("Someone", "Elsewhere", None, None, tracks("X", 1)), None);
    other.songs[0].path = Some("Band/Album/01 Song 1.wav".into());
    other.songs[0].size = files["01 Song 1.wav"] + 1;
    let r = lib.sync(vec![s, other]);
    assert_eq!(r.attached, 4, "{:#?}", lib.rows());
    assert_eq!(
        lib.rows(),
        [
            "Album / Band [folder files] 1-1 Song 1 (file files-1-1)",
            "Album / Band [folder files] 1-2 Song 2 (file files-1-2)",
            "Album / Band [folder files] 1-3 Song 3 (file files-1-3)",
            "Album / Band [folder files] 1-4 Song 4 (file files-1-4)",
            "Elsewhere / Someone [server other] 1-1 X 1 (server other-1-1)",
        ],
        "the files' tags shown; the other server song is its own"
    );
    // The same by file even where the numbers say otherwise (the server numbers them backwards).
    let mut lib2 = Lib::new("same-files-numbers");
    lib2.files("Band/Album", &a, None);
    lib2.scan();
    let mut s = served("rev", &a, "x/Album", &sizes(&lib2, "Band/Album"));
    let n = s.songs.len() as u32;
    for song in &mut s.songs {
        song.track = song.track.map(|t| n + 1 - t);
    }
    assert_eq!(lib2.sync(vec![s]).attached, 4);
    assert_eq!(query::track_by_remote(&lib2.conn, "rev-1-1").unwrap().unwrap().title, "Song 1", "paired by file, not by number");
}

/// A file never overrules what is certain or uncertain otherwise: recording ids that differ keep
/// two tracks (and so two albums), and a file two local albums hold alike is no sign at all.
#[test]
fn the_same_file_never_overrules_recording_ids_or_doubt() {
    let mut lib = Lib::new("files-mb");
    let a = al("Band", "Ids", None, None, vec![tr(1, "Alpha", 100), tr(2, "Beta", 150)]);
    for t in &a.tracks {
        let rel = format!("Band/Ids/{:02} {}.wav", t.no.unwrap(), t.title);
        lib.wav(&rel, &a, t);
        let path = lib.music().join(&rel);
        let mut tagged = lofty::read_from_path(&path).unwrap();
        tagged.primary_tag_mut().unwrap().insert_text(ItemKey::MusicBrainzRecordingId, format!("rec-{}", t.no.unwrap()));
        tagged.save_to_path(&path, WriteOptions::default()).unwrap();
    }
    lib.scan();
    let mut s = served("ids", &a, "Band/Ids", &sizes(&lib, "Band/Ids"));
    s.songs[0].mb_recording_id = Some("rec-1".into());
    s.songs[1].mb_recording_id = Some("rec-other".into());
    lib.sync(vec![s]);
    assert!(query::track_by_remote(&lib.conn, "ids-1-2").unwrap().unwrap().is_remote(), "another recording: not this file's track");
    assert!(lib.rows().iter().all(|r| !r.contains("(file ids-1-2)")), "{:#?}", lib.rows());

    // Two copies of an album in two folders of one name: the same files twice.
    let mut lib = Lib::new("files-twice");
    let b = al("Band", "Twice", None, None, tracks("T", 3));
    lib.files("Disk1/Twice", &b, None);
    lib.files("Disk2/Twice", &b, None);
    lib.scan();
    let s = served("twice", &b, "Twice", &sizes(&lib, "Disk1/Twice"));
    assert_eq!(sizes(&lib, "Disk1/Twice"), sizes(&lib, "Disk2/Twice"));
    let r = lib.sync(vec![s]);
    assert_eq!(r.attached, 0, "either copy could be it");
    assert_eq!(remote::counts(&lib.conn).unwrap().server, 3);
}

/// A server album synced first, its files copied to the music folder later under other tags: the
/// scan's `reconcile` finds it by its files, and the server-only rows become the files' rows.
#[test]
fn files_added_after_a_sync_are_found_by_their_files() {
    let mut lib = Lib::new("files-later");
    let a = al("Band", "Later", Some("2001"), None, tracks("L", 3));
    lib.files("Band/Later", &a, None);
    let files = sizes(&lib, "Band/Later");
    let away = lib.dir.join("away");
    std::fs::rename(lib.music().join("Band"), &away).unwrap();
    lib.scan();
    let mut s = served("later", &a, "Server/Later", &files);
    s.title = "Later (Expanded)".into();
    for song in &mut s.songs {
        song.title.push_str(" (live)");
    }
    lib.sync(vec![s]);
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 3, both: 0 });
    let played = query::track_by_remote(&lib.conn, "later-1-2").unwrap().unwrap().id;
    query::record_play(&lib.conn, played).unwrap();
    std::fs::rename(&away, lib.music().join("Band")).unwrap();
    lib.scan();
    lib.reconcile();
    assert_eq!(remote::counts(&lib.conn).unwrap(), remote::Counts { local: 0, server: 0, both: 3 }, "{:#?}", lib.rows());
    assert_eq!(lib.one("SELECT COUNT(*) FROM albums"), 1);
    let t = query::track_by_remote(&lib.conn, "later-1-2").unwrap().unwrap();
    assert!(!t.is_remote() && t.title == "L 2", "{t:?}");
    assert_eq!(query::play_count(&lib.conn, t.id).unwrap(), 1, "the play counted on the server's row stays with the track");
}

#[test]
fn file_keys_fold_separators_case_and_roots() {
    let k = |p: &str| remote::file_key(p, 10);
    assert_eq!(k(r"M:\Music\Band\Album\01 Song.flac"), k("Band/ALBUM/01 song.FLAC"));
    assert_eq!(k("/home/me/Music/Band/Album/01 Song.flac"), k(r"Album\01 Song.flac"));
    assert_ne!(k("Band/Album/01 Song.flac"), k("Band/Other/01 Song.flac"));
    assert_eq!(k("01 Song.flac"), None, "a bare name says too little");
    assert_eq!(k(r"C:\01 Song.flac"), None, "a drive is no folder");
    assert_eq!(remote::file_key("Band/Album/01.flac", 0), None, "no size, no sign");
    assert_eq!(k("navidrome:song/abc"), None);
}

/// A listen of a track only the server has takes its MusicBrainz ids from what the server said.
#[test]
fn the_servers_musicbrainz_ids_of_a_song() {
    let mut lib = Lib::new("server-mbids");
    let mut a = server("m1", &al("Ann Example", "Ids", Some("2001"), Some("0383DADF-2A4E-4D10-A46A-E9E041DA8EB3"), tracks("Id", 2)), None);
    a.songs[0].mb_recording_id = Some("8f3471b5-7e6a-48da-86a9-c1c07a0f47ae".into());
    a.songs[0].artists[0].mbid = Some("b0a7b4c5-1111-4222-8333-944455556666".into());
    lib.sync(vec![a]);
    let ids = remote::musicbrainz_ids(&lib.conn, "m1-1-1").unwrap();
    assert_eq!(ids.recording.as_deref(), Some("8f3471b5-7e6a-48da-86a9-c1c07a0f47ae"));
    assert_eq!(ids.release.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("0383dadf-2a4e-4d10-a46a-e9e041da8eb3"));
    assert_eq!(ids.artists, ["b0a7b4c5-1111-4222-8333-944455556666"]);
    let none = remote::musicbrainz_ids(&lib.conn, "m1-1-2").unwrap();
    assert_eq!((none.recording, none.artists.len()), (None, 0));
    assert_eq!(remote::musicbrainz_ids(&lib.conn, "nope").unwrap(), Default::default());
}

/// A file's MusicBrainz ids, read for a listen (`tags::musicbrainz_ids`); a server track's path
/// reads nothing.
#[test]
fn a_files_musicbrainz_ids() {
    let lib = Lib::new("file-mbids");
    let a = al("Band", "Ids", Some("2001"), None, vec![tr(1, "Alpha", 100)]);
    let rel = "Band/Ids/01 Alpha.wav";
    lib.wav(rel, &a, &a.tracks[0]);
    let path = lib.music().join(rel);
    let mut tagged = lofty::read_from_path(&path).unwrap();
    let tag = tagged.primary_tag_mut().unwrap();
    for (k, v) in [
        (ItemKey::MusicBrainzRecordingId, "rec-1"),
        (ItemKey::MusicBrainzTrackId, "track-1"),
        (ItemKey::MusicBrainzReleaseId, "rel-1"),
        (ItemKey::MusicBrainzReleaseGroupId, "group-1"),
        (ItemKey::MusicBrainzArtistId, "artist-1"),
    ] {
        tag.insert_text(k, v.into());
    }
    tagged.save_to_path(&path, WriteOptions::default()).unwrap();
    let ids = library::tags::musicbrainz_ids(&path);
    assert_eq!(
        ids,
        library::tags::MusicBrainzIds {
            recording: Some("rec-1".into()),
            release_track: Some("track-1".into()),
            release: Some("rel-1".into()),
            release_group: Some("group-1".into()),
            artists: vec!["artist-1".into()],
        }
    );
    assert_eq!(library::tags::musicbrainz_ids(std::path::Path::new(&remote::song_path("s1"))), Default::default());
}
