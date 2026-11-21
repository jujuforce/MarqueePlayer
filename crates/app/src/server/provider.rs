//! Read operations shared by catalog sync, cover fetching and playback.
//! Only Subsonic exposes a write client and server lyrics.

mod subsonic_catalog;

pub(super) use subsonic_catalog::star_time;

use http_client::audio::{StreamSink, Streamed};
use library::remote::RemoteAlbum;
use subsonic::Error;

use super::plex;

#[derive(Clone)]
pub enum Client {
    Subsonic(subsonic::Client),
    Plex(plex::Client),
}

pub fn is_plex_identity(identity: &str) -> bool {
    identity.starts_with("plex:")
}

/// A Subsonic listing spans the whole server. Plex lists each music section separately.
#[derive(PartialEq, Eq)]
pub(super) struct CatalogState {
    pub sections: Vec<Option<String>>,
    pub scanning: bool,
    pub last_scan: Option<String>,
}

pub(super) struct ListedAlbum {
    pub album: RemoteAlbum,
    pub song_count: u32,
}

impl Client {
    pub fn as_subsonic(&self) -> Option<&subsonic::Client> {
        match self {
            Self::Subsonic(c) => Some(c),
            Self::Plex(_) => None,
        }
    }

    pub fn same_credentials(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Subsonic(a), Self::Subsonic(b)) => a.credentials() == b.credentials(),
            (Self::Plex(a), Self::Plex(b)) => a.same_credentials(b),
            _ => false,
        }
    }

    pub fn set_stream_timeout_ms(&mut self, timeout_ms: u32) {
        match self {
            Self::Subsonic(c) => c.stream_timeout_ms = timeout_ms,
            Self::Plex(c) => c.stream_timeout_ms = timeout_ms,
        }
    }

    /// A bitrate requests a progressive MP3 in kbps. Without one, play the original file.
    pub fn stream_quality(&self, id: &str, bitrate: Option<u32>, from: u64, validator: Option<&str>, sink: &mut dyn StreamSink) -> Result<Streamed, Error> {
        match self {
            Self::Subsonic(c) => {
                let quality = bitrate.map_or(subsonic::Quality::Original, |max_bit_rate| subsonic::Quality::Transcode { format: "mp3".into(), max_bit_rate });
                c.stream(id, &quality, from, validator, sink)
            }
            Self::Plex(c) => c.stream_quality(id, bitrate, from, validator, sink),
        }
    }

    pub(super) fn catalog_state(&self) -> Result<CatalogState, Error> {
        match self {
            Self::Subsonic(c) => {
                let status = c.scan_status()?;
                Ok(CatalogState { sections: vec![None], scanning: status.scanning, last_scan: status.last_scan })
            }
            Self::Plex(c) => c.catalog_state(),
        }
    }

    pub(super) fn albums(&self, section: Option<&str>, size: u32, offset: u32) -> Result<Vec<ListedAlbum>, Error> {
        match self {
            Self::Subsonic(c) => Ok(c.album_list2(&subsonic::AlbumList::AlphabeticalByName, size, offset, section)?
                .into_iter()
                .map(|a| ListedAlbum { album: RemoteAlbum { id: a.id, ..Default::default() }, song_count: a.song_count })
                .collect()),
            Self::Plex(c) => c.albums(section.ok_or_else(|| Error::BadResponse("missing Plex music section".into()))?, size, offset),
        }
    }

    pub(super) fn album(&self, listed: &ListedAlbum) -> Result<RemoteAlbum, Error> {
        match self {
            Self::Subsonic(c) => {
                let album = c.album(&listed.album.id)?;
                // Some Subsonic servers count non-music entries. Keep the existing
                // tolerance, but do not remove songs based on a clearly truncated album.
                let expected = album.song_count.max(listed.song_count);
                if album.song.len() * 2 < expected as usize {
                    return Err(short_album(album.song.len(), expected));
                }
                Ok(subsonic_catalog::album_of(&album))
            }
            Self::Plex(c) => {
                let album = c.album(listed)?;
                if album.songs.len() < listed.song_count as usize {
                    return Err(short_album(album.songs.len(), listed.song_count));
                }
                Ok(album)
            }
        }
    }

    pub(super) fn cover(&self, id: &str, size: u32) -> Result<Vec<u8>, Error> {
        match self {
            Self::Subsonic(c) => Ok(c.cover_art(id, Some(size))?.bytes),
            Self::Plex(c) => c.cover(id, size),
        }
    }
}

fn short_album(read: usize, expected: u32) -> Error {
    Error::BadResponse(format!("the album came with {read} of its {expected} songs"))
}
