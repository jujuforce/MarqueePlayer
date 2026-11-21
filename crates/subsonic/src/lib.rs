//! A Subsonic / OpenSubsonic client for Marquee Player's server library source (written
//! against Navidrome, read tolerantly for other servers). Blocking calls over the shared HTTP
//! client (`http-client`: WinHTTP on Windows, ureq elsewhere); no UI and no threads of its
//! own: callers run it on worker threads.
//!
//! - [`sign_in`]: the address as typed ([`normalize_url`], [`is_plain_http`] for the UI's
//!   warning), the user and password, to [`Credentials`] to store (salt and token; the
//!   password is never kept) and what the server is ([`ServerInfo`]).
//! - [`Client`]: typed reads (artists, albums, songs, `search3` for whole-catalog paging,
//!   playlists, genres, starred, lyrics, scan status), [`Client::cover_art`] as bytes, and
//!   [`Client::stream`] handing a song to a sink as it downloads, from a byte offset when
//!   the server allows.
//! - Writes (star, scrobble, playlists) exist, each kind allowed or not by the client's
//!   [`Writes`] ([`Client::with_writes`]); a write not allowed is refused before a request is
//!   made ([`Error::ReadOnly`]). `Client::new(.., true)` allows none. None of them touches a
//!   music file: they change the server's own database only.
//! - [`Error`]: classes the UI can word (bad address, no connection, not a Subsonic server,
//!   wrong password, not permitted, not found, server error, unreadable answer).
//!
//! Every request carries `u`, `t` = md5(password + salt), `s`, `v` = 1.16.1,
//! `c` = MarqueePlayer and `f` = json. Since the token and salt are in every URL, no URL is
//! logged or put in an error; log lines name the endpoint and the outcome.
//!
//! What a stream does with a range, as the Android client found on Navidrome: the original
//! file (`format=raw`) is served with ranges and a length; a live transcode has neither (no
//! `Content-Length`, ranges ignored, always from the start), so [`StreamHead`] says where the
//! body really starts.

mod auth;
mod client;
mod de;
mod error;
#[cfg(feature = "mock")]
pub mod mock;
pub mod models;

pub use auth::{Credentials, is_plain_http, new_salt, normalize_url, token};
pub use client::{
    API_VERSION, AlbumList, CLIENT_NAME, Client, Cover, MARK_ENDPOINTS, PLAY_ENDPOINTS, PLAYLIST_ENDPOINTS, PlaylistUpdate, Quality, READ_ENDPOINTS, STREAM_TIMEOUT_MS,
    Search, TIMEOUT_MS, Writes, sign_in,
};
pub use error::Error;
pub use http_client::audio::{StreamHead, StreamSink, Streamed};
pub use models::*;
