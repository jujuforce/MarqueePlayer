//! The calls: one blocking HTTP GET each, on the caller's thread, signed with the stored
//! token. Every request URL carries the token and salt, so a URL is never logged, put in an
//! error, or shown; log lines name the endpoint and the outcome only.

use http_client::Request;
use http_client::audio::{self, StreamError, StreamSink, Streamed};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{Credentials, normalize_url};
use crate::de;
use crate::models::*;
use crate::Error;

/// The Subsonic API version asked for: 1.16.1, the last, which Navidrome speaks.
pub const API_VERSION: &str = "1.16.1";
/// The `c` parameter: the name the server shows for this client (Navidrome's player list).
pub const CLIENT_NAME: &str = "MarqueePlayer";
/// For each of connect, send and each read of an answer.
pub const TIMEOUT_MS: u32 = 20_000;
/// For a cover or a song: each read of the body (the whole may take minutes). Longer than
/// `TIMEOUT_MS`: a server starting a transcode can take a while before the first byte.
pub const STREAM_TIMEOUT_MS: u32 = 30_000;

/// The endpoints a read-only client may call; every other is refused before a request is
/// built. Writes (star, rate, scrobble, playlists) are outside it by design.
pub const READ_ENDPOINTS: &[&str] = &[
    "ping",
    "getOpenSubsonicExtensions",
    "getScanStatus",
    "getMusicFolders",
    "getArtists",
    "getArtist",
    "getAlbumList2",
    "getAlbum",
    "getSong",
    "search3",
    "getPlaylists",
    "getPlaylist",
    "getGenres",
    "getStarred2",
    "getLyricsBySongId",
    "getCoverArt",
    "stream",
];

/// The write endpoints of each kind a client may be allowed ([`Writes`]).
pub const MARK_ENDPOINTS: &[&str] = &["star", "unstar"];
pub const PLAY_ENDPOINTS: &[&str] = &["scrobble"];
pub const PLAYLIST_ENDPOINTS: &[&str] = &["createPlaylist", "updatePlaylist", "deletePlaylist"];

/// Which kinds of write a client may send; every other is refused before a request is built
/// ([`Error::ReadOnly`]). Reads ([`READ_ENDPOINTS`]) are always allowed. None of these writes
/// touch a music file: they change only the server's own database (its users' stars, plays and
/// playlists).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Writes {
    /// `star`, `unstar`.
    pub marks: bool,
    /// `scrobble` (a play, or "now playing").
    pub plays: bool,
    /// `createPlaylist`, `updatePlaylist`, `deletePlaylist`.
    pub playlists: bool,
}

impl Writes {
    /// Read-only: no write at all.
    pub const NONE: Writes = Writes { marks: false, plays: false, playlists: false };
    pub const ALL: Writes = Writes { marks: true, plays: true, playlists: true };

    pub fn any(self) -> bool {
        self.marks || self.plays || self.playlists
    }

    /// Whether `endpoint` may be called: a read, or a write of a kind allowed.
    pub fn allows(self, endpoint: &str) -> bool {
        READ_ENDPOINTS.contains(&endpoint)
            || (self.marks && MARK_ENDPOINTS.contains(&endpoint))
            || (self.plays && PLAY_ENDPOINTS.contains(&endpoint))
            || (self.playlists && PLAYLIST_ENDPOINTS.contains(&endpoint))
    }
}

/// Which albums `getAlbumList2` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlbumList {
    /// Recently added.
    Newest,
    /// Recently played.
    Recent,
    /// Most played.
    Frequent,
    Random,
    /// Highest rated.
    Highest,
    Starred,
    AlphabeticalByName,
    AlphabeticalByArtist,
    /// Released between the two years, inclusive (from > to lists newest first).
    ByYear { from: i32, to: i32 },
    ByGenre(String),
}

/// One `search3` page. Counts of 0 ask for none of that kind; the server's default for a
/// count not sent is 20, so all six are always sent.
///
/// An empty query is how a whole catalog is listed: Navidrome answers it with everything,
/// paged by the counts and offsets (ordered by id, so pages are stable while nothing
/// changes). Page until a kind comes back shorter than its count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search<'a> {
    pub query: &'a str,
    pub artist_count: u32,
    pub artist_offset: u32,
    pub album_count: u32,
    pub album_offset: u32,
    pub song_count: u32,
    pub song_offset: u32,
    pub music_folder_id: Option<&'a str>,
}

impl<'a> Search<'a> {
    /// Songs only: `count` from `offset`.
    pub fn songs(query: &'a str, count: u32, offset: u32) -> Self {
        Search { query, song_count: count, song_offset: offset, ..Search::default() }
    }
    /// Albums only.
    pub fn albums(query: &'a str, count: u32, offset: u32) -> Self {
        Search { query, album_count: count, album_offset: offset, ..Search::default() }
    }
    /// Artists only.
    pub fn artists(query: &'a str, count: u32, offset: u32) -> Self {
        Search { query, artist_count: count, artist_offset: offset, ..Search::default() }
    }
}

/// What `stream` sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quality {
    /// The file as it is (`format=raw`): ranges work, the length is known.
    Original,
    /// Transcoded live (`format`, at most `max_bit_rate` kbps; 0 for the server's default).
    /// Navidrome sends a live transcode with no length and ignores ranges: it always
    /// starts at the beginning.
    Transcode { format: String, max_bit_rate: u32 },
}

/// A cover image as the server sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub bytes: Vec<u8>,
    /// "image/jpeg", "image/png", "image/webp"; None when the server did not say.
    pub content_type: Option<String>,
}

/// What `updatePlaylist` changes; empty fields are left as they are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistUpdate<'a> {
    pub name: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub public: Option<bool>,
    /// Song ids appended, in order.
    pub add: &'a [&'a str],
    /// Positions (0-based, in the playlist as it was) removed.
    pub remove: &'a [u32],
}

/// A client for one server and user. Cheap to clone; holds no connection or thread of its
/// own (callers run it on worker threads). `Debug` hides the credentials.
#[derive(Clone)]
pub struct Client {
    /// The normalized base URL ("https://host/navidrome"), without a trailing slash.
    base: String,
    credentials: Credentials,
    /// The writes it may send; every other write is refused before a request is made
    /// ([`Writes::NONE`]: read-only, only [`READ_ENDPOINTS`]).
    pub writes: Writes,
    /// The HTTP User-Agent (`MarqueePlayer/<version>`).
    pub user_agent: String,
    pub timeout_ms: u32,
    pub stream_timeout_ms: u32,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("credentials", &self.credentials).field("writes", &self.writes).finish_non_exhaustive()
    }
}

type Params = Vec<(&'static str, String)>;

/// Sign in: make a fresh salt and its token from the password, ping the server with them,
/// and give back the credentials to store and what the server is. The password is not
/// kept. Every error class can come back; [`Error::WrongCredentials`] and
/// [`Error::NotSubsonic`] are the ones a user fixes by retyping.
pub fn sign_in(url: &str, user: &str, password: &str) -> Result<(Credentials, ServerInfo), Error> {
    let credentials = Credentials::from_password(url, user, password)?;
    let info = Client::new(credentials.clone(), true)?.ping()?;
    Ok((credentials, info))
}

impl Client {
    /// A client for `credentials` (its URL is normalized again, so one stored by hand works
    /// too). Nothing is read from the environment: tests and the bench point it at a mock by
    /// its URL. Nothing is sent until a call. `read_only`: no write at all ([`Writes::NONE`]);
    /// else every write ([`Writes::ALL`]). [`Client::with_writes`] allows some kinds only.
    pub fn new(credentials: Credentials, read_only: bool) -> Result<Self, Error> {
        let base = normalize_url(&credentials.url)?;
        Ok(Client {
            base,
            credentials,
            writes: if read_only { Writes::NONE } else { Writes::ALL },
            user_agent: format!("{CLIENT_NAME}/{}", env!("CARGO_PKG_VERSION")),
            timeout_ms: TIMEOUT_MS,
            stream_timeout_ms: STREAM_TIMEOUT_MS,
        })
    }

    /// The same client, allowed these writes only.
    pub fn with_writes(mut self, writes: Writes) -> Self {
        self.writes = writes;
        self
    }

    /// No write of any kind can be sent.
    pub fn is_read_only(&self) -> bool {
        !self.writes.any()
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// The normalized base URL (no secrets in it).
    pub fn base_url(&self) -> &str {
        &self.base
    }

    // ---- reads -----------------------------------------------------------------------

    /// What the server is; also the check that the credentials work.
    pub fn ping(&self) -> Result<ServerInfo, Error> {
        let env = self.call("ping", Vec::new())?;
        ServerInfo::deserialize(&env).map_err(|e| Error::BadResponse(format!("ping: {e}")))
    }

    /// The OpenSubsonic extensions the server supports (empty on a server that has none).
    pub fn open_subsonic_extensions(&self) -> Result<Vec<Extension>, Error> {
        self.list("getOpenSubsonicExtensions", "openSubsonicExtensions", None, Vec::new())
    }

    pub fn scan_status(&self) -> Result<ScanStatus, Error> {
        self.object("getScanStatus", "scanStatus", Vec::new())
    }

    pub fn music_folders(&self) -> Result<Vec<MusicFolder>, Error> {
        self.list("getMusicFolders", "musicFolders", Some("musicFolder"), Vec::new())
    }

    /// Every artist, by letter (album artists, on Navidrome by default).
    pub fn artists(&self, music_folder_id: Option<&str>) -> Result<Artists, Error> {
        self.object("getArtists", "artists", opt(vec![], "musicFolderId", music_folder_id))
    }

    /// An artist with its albums.
    pub fn artist(&self, id: &str) -> Result<Artist, Error> {
        self.object("getArtist", "artist", vec![("id", id.to_owned())])
    }

    /// A page of albums: `size` (the server caps it at 500) from `offset`.
    pub fn album_list2(&self, list: &AlbumList, size: u32, offset: u32, music_folder_id: Option<&str>) -> Result<Vec<Album>, Error> {
        let (kind, mut p): (&str, Params) = match list {
            AlbumList::Newest => ("newest", vec![]),
            AlbumList::Recent => ("recent", vec![]),
            AlbumList::Frequent => ("frequent", vec![]),
            AlbumList::Random => ("random", vec![]),
            AlbumList::Highest => ("highest", vec![]),
            AlbumList::Starred => ("starred", vec![]),
            AlbumList::AlphabeticalByName => ("alphabeticalByName", vec![]),
            AlbumList::AlphabeticalByArtist => ("alphabeticalByArtist", vec![]),
            AlbumList::ByYear { from, to } => ("byYear", vec![("fromYear", from.to_string()), ("toYear", to.to_string())]),
            AlbumList::ByGenre(g) => ("byGenre", vec![("genre", g.clone())]),
        };
        p.insert(0, ("type", kind.to_owned()));
        p.push(("size", size.to_string()));
        p.push(("offset", offset.to_string()));
        self.list("getAlbumList2", "albumList2", Some("album"), opt(p, "musicFolderId", music_folder_id))
    }

    /// An album with its songs.
    pub fn album(&self, id: &str) -> Result<Album, Error> {
        self.object("getAlbum", "album", vec![("id", id.to_owned())])
    }

    pub fn song(&self, id: &str) -> Result<Song, Error> {
        self.object("getSong", "song", vec![("id", id.to_owned())])
    }

    /// One page of a search; see [`Search`] for listing a whole catalog.
    pub fn search3(&self, s: &Search) -> Result<SearchResult, Error> {
        let p = vec![
            ("query", s.query.to_owned()),
            ("artistCount", s.artist_count.to_string()),
            ("artistOffset", s.artist_offset.to_string()),
            ("albumCount", s.album_count.to_string()),
            ("albumOffset", s.album_offset.to_string()),
            ("songCount", s.song_count.to_string()),
            ("songOffset", s.song_offset.to_string()),
        ];
        self.object_or_default("search3", "searchResult3", opt(p, "musicFolderId", s.music_folder_id))
    }

    /// The user's playlists (and the public ones), without their songs.
    pub fn playlists(&self) -> Result<Vec<Playlist>, Error> {
        self.list("getPlaylists", "playlists", Some("playlist"), Vec::new())
    }

    /// A playlist with its songs.
    pub fn playlist(&self, id: &str) -> Result<Playlist, Error> {
        self.object("getPlaylist", "playlist", vec![("id", id.to_owned())])
    }

    pub fn genres(&self) -> Result<Vec<Genre>, Error> {
        self.list("getGenres", "genres", Some("genre"), Vec::new())
    }

    /// The user's starred artists, albums and songs.
    pub fn starred2(&self, music_folder_id: Option<&str>) -> Result<SearchResult, Error> {
        // An answer without it is no answer, not "nothing starred": read as nothing starred, it
        // would unstar every favourite.
        self.object("getStarred2", "starred2", opt(vec![], "musicFolderId", music_folder_id))
    }

    /// OpenSubsonic `getLyricsBySongId` (the `songLyrics` extension): every lyrics entry the
    /// server has for the song; empty when it has none.
    pub fn lyrics_by_song_id(&self, id: &str) -> Result<Vec<StructuredLyrics>, Error> {
        self.list("getLyricsBySongId", "lyricsList", Some("structuredLyrics"), vec![("id", id.to_owned())])
    }

    // ---- binary ----------------------------------------------------------------------

    /// A cover (`coverArt` of a song, album, artist or playlist), at `size` pixels on its
    /// long side when given (the server scales and caches it), else as stored.
    pub fn cover_art(&self, id: &str, size: Option<u32>) -> Result<Cover, Error> {
        let endpoint = "getCoverArt";
        let url = self.url(endpoint, &opt(vec![("id", id.to_owned())], "size", size.map(|s| s.to_string()).as_deref()))?;
        let r = http_client::request_bytes_same_server(&Request { method: "GET", url: &url, headers: &[], body: &[], user_agent: &self.user_agent, timeout_ms: self.stream_timeout_ms })
            .map_err(|e| self.failed(endpoint, Error::Network(e.to_string())))?;
        if let Some(e) = self.binary_error(r.status, r.content_type.as_deref(), &r.body) {
            return Err(self.failed(endpoint, e));
        }
        if r.body.is_empty() {
            return Err(self.failed(endpoint, Error::BadResponse("an empty image".into())));
        }
        log::debug!("subsonic {endpoint}: {} bytes", r.body.len());
        Ok(Cover { bytes: r.body, content_type: r.content_type })
    }

    /// A song's audio, from byte `range_start` when the server allows (`Range`), handed to
    /// `sink` as it arrives and never held whole. The sink is told first where the body
    /// starts (`StreamHead`): from `range_start` when the range was honoured, else from 0.
    ///
    /// An error answer (a JSON error, an error status, a web page) never reaches the sink:
    /// it comes back as the call's error. A connection lost mid-body is [`Error::Network`]
    /// after the sink has had every byte that came. A `range_start` at or past the end of
    /// the file is an empty, finished stream.
    ///
    /// `if_range`: the validator an earlier answer gave ([`crate::StreamHead::validator`]), sent as
    /// `If-Range` with the `Range`, so the server sends the range only if the file is still that
    /// one, and else the whole file from its start. Not sent without a range.
    pub fn stream(&self, id: &str, quality: &Quality, range_start: u64, if_range: Option<&str>, sink: &mut dyn StreamSink) -> Result<Streamed, Error> {
        let endpoint = "stream";
        let mut p = vec![("id", id.to_owned())];
        match quality {
            Quality::Original => p.push(("format", "raw".to_owned())),
            Quality::Transcode { format, max_bit_rate } => {
                p.push(("format", format.clone()));
                p.push(("maxBitRate", max_bit_rate.to_string()));
            }
        }
        let url = self.url(endpoint, &p)?;
        let req = Request { method: "GET", url: &url, headers: &[], body: &[], user_agent: &self.user_agent, timeout_ms: self.stream_timeout_ms };
        let streamed = audio::download(&req, range_start, if_range, sink).map_err(|e| {
            let e = match e {
                StreamError::Network(e) => Error::Network(e.to_string()),
                StreamError::BadRange => Error::BadResponse("invalid Content-Range".into()),
                StreamError::Response { status, content_type, body } => self.binary_error(status, content_type.as_deref(), &body)
                    .unwrap_or_else(|| Error::BadResponse("an error answer without an error".into())),
            };
            self.failed(endpoint, e)
        })?;
        log::debug!("subsonic {endpoint}: from {} ({}), {} bytes{}", streamed.head.start, if streamed.head.range_honoured { "as asked" } else { "range ignored" }, streamed.received, if streamed.stopped { ", stopped" } else { "" });
        Ok(streamed)
    }

    // ---- writes (refused when read-only) ---------------------------------------------

    /// Star songs, albums and artists (any of the lists may be empty).
    pub fn star(&self, song_ids: &[&str], album_ids: &[&str], artist_ids: &[&str]) -> Result<(), Error> {
        self.call("star", star_params(song_ids, album_ids, artist_ids)).map(drop)
    }

    pub fn unstar(&self, song_ids: &[&str], album_ids: &[&str], artist_ids: &[&str]) -> Result<(), Error> {
        self.call("unstar", star_params(song_ids, album_ids, artist_ids)).map(drop)
    }

    /// A play: `submission` true for a play to count (at `time_ms`, Unix milliseconds, when
    /// it started; the server's now when None), false for "now playing".
    pub fn scrobble(&self, id: &str, time_ms: Option<i64>, submission: bool) -> Result<(), Error> {
        let mut p = vec![("id", id.to_owned())];
        if let Some(t) = time_ms {
            p.push(("time", t.to_string()));
        }
        p.push(("submission", submission.to_string()));
        self.call("scrobble", p).map(drop)
    }

    /// Several plays in one call: (song id, Unix milliseconds) each, `id` and `time` repeated
    /// in the same order (Navidrome takes this).
    pub fn scrobble_plays(&self, plays: &[(&str, i64)]) -> Result<(), Error> {
        let mut p: Params = plays.iter().map(|(id, _)| ("id", (*id).to_owned())).collect();
        p.extend(plays.iter().map(|(_, t)| ("time", t.to_string())));
        p.push(("submission", "true".to_owned()));
        self.call("scrobble", p).map(drop)
    }

    /// A new playlist of these songs. The server's answer (1.14.0 and later) is the playlist.
    pub fn create_playlist(&self, name: &str, song_ids: &[&str]) -> Result<Option<Playlist>, Error> {
        let mut p = vec![("name", name.to_owned())];
        p.extend(song_ids.iter().map(|id| ("songId", (*id).to_owned())));
        self.get("createPlaylist", "playlist", p)
    }

    pub fn update_playlist(&self, id: &str, u: &PlaylistUpdate) -> Result<(), Error> {
        let mut p = vec![("playlistId", id.to_owned())];
        p = opt(p, "name", u.name);
        p = opt(p, "comment", u.comment);
        if let Some(public) = u.public {
            p.push(("public", public.to_string()));
        }
        p.extend(u.add.iter().map(|s| ("songIdToAdd", (*s).to_owned())));
        p.extend(u.remove.iter().map(|i| ("songIndexToRemove", i.to_string())));
        self.call("updatePlaylist", p).map(drop)
    }

    pub fn delete_playlist(&self, id: &str) -> Result<(), Error> {
        self.call("deletePlaylist", vec![("id", id.to_owned())]).map(drop)
    }

    // ---- plumbing --------------------------------------------------------------------

    /// The request URL, after the read-only guard. Holds the token: never logged.
    fn url(&self, endpoint: &'static str, params: &[(&'static str, String)]) -> Result<String, Error> {
        if !self.writes.allows(endpoint) {
            log::info!("subsonic {endpoint}: refused, the connection may not send it");
            return Err(Error::ReadOnly(endpoint));
        }
        let c = &self.credentials;
        let mut q: Vec<(String, String)> = vec![
            ("u".into(), c.user.clone()),
            ("t".into(), c.token.clone()),
            ("s".into(), c.salt.clone()),
            ("v".into(), API_VERSION.into()),
            ("c".into(), CLIENT_NAME.into()),
            ("f".into(), "json".into()),
        ];
        q.extend(params.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        Ok(format!("{}/rest/{endpoint}.view?{}", self.base, http_client::form_encode(&q)))
    }

    /// Text from elsewhere with the token and salt masked, should it echo them (an HTTP
    /// layer's message, a proxy's error page, a server's message).
    fn mask(&self, s: &str) -> String {
        let mut s = s.to_owned();
        for secret in [&self.credentials.token, &self.credentials.salt] {
            if secret.len() >= 4 {
                s = s.replace(secret.as_str(), "<hidden>");
            }
        }
        s
    }

    /// Mask and log an error on its way out.
    fn failed(&self, endpoint: &str, e: Error) -> Error {
        let e = match e {
            Error::Network(m) => Error::Network(self.mask(&m)),
            Error::NotSubsonic(m) => Error::NotSubsonic(self.mask(&m)),
            Error::WrongCredentials { code, message } => Error::WrongCredentials { code, message: self.mask(&message) },
            Error::NotPermitted(m) => Error::NotPermitted(self.mask(&m)),
            Error::NotFound(m) => Error::NotFound(self.mask(&m)),
            Error::Server { code, http_status, message } => Error::Server { code, http_status, message: self.mask(&message) },
            Error::BadResponse(m) => Error::BadResponse(self.mask(&m)),
            e @ (Error::BadUrl | Error::ReadOnly(_)) => e,
        };
        log::info!("subsonic {endpoint}: {e}");
        e
    }

    /// One call: the `subsonic-response` envelope of a successful answer.
    fn call(&self, endpoint: &'static str, params: Params) -> Result<Value, Error> {
        let url = self.url(endpoint, &params)?;
        let r = http_client::request_bytes_same_server(&Request { method: "GET", url: &url, headers: &[], body: &[], user_agent: &self.user_agent, timeout_ms: self.timeout_ms })
            .map_err(|e| self.failed(endpoint, Error::Network(e.to_string())))?;
        let env = envelope(r.status, &r.body).map_err(|e| self.failed(endpoint, e))?;
        log::debug!("subsonic {endpoint}: ok ({} bytes)", r.body.len());
        Ok(env)
    }

    /// The value under `key`, read; None when the answer has none.
    fn get<T: DeserializeOwned>(&self, endpoint: &'static str, key: &str, params: Params) -> Result<Option<T>, Error> {
        let env = self.call(endpoint, params)?;
        match env.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => T::deserialize(v).map(Some).map_err(|e| self.failed(endpoint, Error::BadResponse(format!("{endpoint}: {e}")))),
        }
    }

    /// The object under `key`, which the call must have.
    fn object<T: DeserializeOwned>(&self, endpoint: &'static str, key: &str, params: Params) -> Result<T, Error> {
        self.get(endpoint, key, params)?.ok_or_else(|| self.failed(endpoint, Error::BadResponse(format!("{endpoint}: no {key} in the answer"))))
    }

    /// The object under `key`, or its default when the answer has none (an empty result).
    fn object_or_default<T: DeserializeOwned + Default>(&self, endpoint: &'static str, key: &str, params: Params) -> Result<T, Error> {
        Ok(self.get(endpoint, key, params)?.unwrap_or_default())
    }

    /// A list under `key` (and `inner`, for the `{"albumList2": {"album": [...]}}` shape),
    /// read tolerantly; empty when the answer has none.
    fn list<T: DeserializeOwned>(&self, endpoint: &'static str, key: &str, inner: Option<&str>, params: Params) -> Result<Vec<T>, Error> {
        let env = self.call(endpoint, params)?;
        let v = env.get(key).and_then(|v| match inner {
            Some(i) => v.get(i),
            None => Some(v),
        });
        #[derive(Deserialize)]
        #[serde(bound = "T: DeserializeOwned")]
        struct Items<T>(#[serde(deserialize_with = "de::list")] Vec<T>);
        match v {
            None => Ok(Vec::new()),
            Some(v) => Items::<T>::deserialize(v).map(|i| i.0).map_err(|e| self.failed(endpoint, Error::BadResponse(format!("{endpoint}: {e}")))),
        }
    }

    /// The error in a cover's or stream's answer, if it is one: a non-2xx status, or a JSON
    /// or XML or text body where audio or an image belongs.
    fn binary_error(&self, status: u16, content_type: Option<&str>, body: &[u8]) -> Option<Error> {
        let ct = content_type.unwrap_or("").to_ascii_lowercase();
        let not_binary = ct.contains("json") || ct.contains("xml") || ct.starts_with("text/");
        if (200..300).contains(&status) && !not_binary {
            return None;
        }
        Some(match envelope(status, body) {
            // A file endpoint failing with a bare status: the server is a Subsonic one (it
            // got this far); say what the status says.
            Err(Error::NotSubsonic(_)) if matches!(status, 404 | 410) => Error::NotFound(format!("HTTP {status}")),
            Err(Error::NotSubsonic(_)) if !(200..300).contains(&status) => Error::Server { code: None, http_status: Some(status), message: String::new() },
            Err(e) => e,
            Ok(_) if (200..300).contains(&status) => Error::BadResponse("a Subsonic answer where a file belongs".into()),
            Ok(_) => Error::Server { code: None, http_status: Some(status), message: String::new() },
        })
    }
}

/// Add `key` when there is a value.
fn opt(mut p: Params, key: &'static str, value: Option<&str>) -> Params {
    if let Some(v) = value {
        p.push((key, v.to_owned()));
    }
    p
}

fn star_params(song_ids: &[&str], album_ids: &[&str], artist_ids: &[&str]) -> Params {
    let ids = |k: &'static str, l: &[&str]| l.iter().map(move |v| (k, (*v).to_owned())).collect::<Vec<_>>();
    [ids("id", song_ids), ids("albumId", album_ids), ids("artistId", artist_ids)].concat()
}

/// The `subsonic-response` of an answer, when it is a successful one; else the error, sorted.
/// The body's text is never put in a message (a proxy's page could echo the URL).
fn envelope(status: u16, body: &[u8]) -> Result<Value, Error> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    let Some(env) = json.and_then(|mut j| j.get_mut("subsonic-response").map(Value::take)).filter(Value::is_object) else {
        if (500..600).contains(&status) {
            return Err(Error::Server { code: None, http_status: Some(status), message: String::new() });
        }
        let what = match body.iter().find(|b| !b.is_ascii_whitespace()) {
            None => "an empty answer",
            Some(b'<') => "a web page or XML",
            Some(b'{' | b'[') => "JSON of another kind",
            Some(_) => "an answer that is not JSON",
        };
        return Err(Error::NotSubsonic(if (200..300).contains(&status) { what.to_owned() } else { format!("HTTP {status}, {what}") }));
    };
    if env.get("status").and_then(Value::as_str) == Some("ok") {
        return Ok(env);
    }
    let error = env.get("error");
    let code = error.and_then(|e| e.get("code")).and_then(|c| c.as_i64().or_else(|| c.as_str()?.trim().parse().ok()));
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("").to_owned();
    let http_status = (!(200..300).contains(&status)).then_some(status);
    Err(match code {
        Some(c) => Error::from_code(c, message, http_status),
        None => Error::Server { code: None, http_status, message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_sort_into_classes() {
        let ok = br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        assert!(envelope(200, ok).is_ok());
        let failed = |code: i64| format!(r#"{{"subsonic-response":{{"status":"failed","error":{{"code":{code},"message":"m"}}}}}}"#);
        assert!(matches!(envelope(200, failed(40).as_bytes()), Err(Error::WrongCredentials { code: 40, .. })));
        assert_eq!(envelope(200, failed(70).as_bytes()), Err(Error::NotFound("m".into())));
        assert_eq!(envelope(200, failed(50).as_bytes()), Err(Error::NotPermitted("m".into())));
        assert_eq!(envelope(404, b"<html>nope</html>"), Err(Error::NotSubsonic("HTTP 404, a web page or XML".into())));
        assert_eq!(envelope(200, b"<html></html>"), Err(Error::NotSubsonic("a web page or XML".into())));
        assert_eq!(envelope(200, br#"{"hello":1}"#), Err(Error::NotSubsonic("JSON of another kind".into())));
        assert_eq!(envelope(200, b"\x00\x01garbage"), Err(Error::NotSubsonic("an answer that is not JSON".into())));
        assert_eq!(envelope(502, b"Bad gateway"), Err(Error::Server { code: None, http_status: Some(502), message: String::new() }));
    }

    #[test]
    fn every_read_endpoint_is_a_read() {
        for w in ["star", "unstar", "scrobble", "createPlaylist", "updatePlaylist", "deletePlaylist"] {
            assert!(!READ_ENDPOINTS.contains(&w), "{w}");
            assert!(!Writes::NONE.allows(w) && Writes::ALL.allows(w), "{w}");
        }
    }

    #[test]
    fn each_kind_of_write_is_allowed_alone() {
        let marks = Writes { marks: true, ..Writes::NONE };
        let plays = Writes { plays: true, ..Writes::NONE };
        assert!(marks.allows("star") && marks.allows("unstar") && marks.allows("getAlbum"));
        assert!(!Writes::ALL.allows("setRating"), "ratings are never sent");
        assert!(!marks.allows("scrobble") && !marks.allows("createPlaylist"));
        assert!(plays.allows("scrobble") && !plays.allows("star") && !plays.allows("deletePlaylist"));
        assert!(!Writes::ALL.allows("startScan"), "an endpoint known to none is refused");
        assert!(!Writes::NONE.any() && marks.any());
    }
}
