//! Map Plex library sections, albums and tracks into the existing remote catalog.
//! Plex serializes some integer fields as strings, especially in older libraries.

use std::collections::HashSet;
use std::path::Path;

use library::remote::{RemoteAlbum, RemoteSong};
use serde_json::Value;
use subsonic::Error;

use super::{Client, MusicLibrary, bad, is_id, media_path};
use super::super::provider::{CatalogState, ListedAlbum};

impl Client {
    /// A missing or inaccessible selection has no catalog. The caller can still show the
    /// available libraries and must keep any previously synced music until a selection exists.
    fn read_libraries(&self, timeout_ms: u32) -> Result<(Vec<MusicLibrary>, Option<CatalogState>), Error> {
        let container = self.json_with_timeout("/library/sections", timeout_ms, Some(&self.token))?;
        let mut libraries = Vec::new();
        let mut catalog = None;
        for section in items(&container, "Directory")? {
            if section["type"].as_str() != Some("artist") {
                continue;
            }
            let key = identifier(&section["key"])?;
            if self.library_key.as_deref() == Some(&key) {
                let refreshing = &section["refreshing"];
                let scanning = refreshing.as_bool() == Some(true) || refreshing == "true" || number(refreshing) == Some(1);
                // Only the selected library's state decides whether removals are safe.
                let revision = ["updatedAt", "scannedAt", "contentChangedAt"].map(|field| number(&section[field]));
                catalog = Some(CatalogState { sections: vec![Some(key.clone())], scanning, last_scan: Some(format!("{key}:{revision:?}")) });
            }
            let title = string(section, "title").unwrap_or_else(|| format!("Music library {key}"));
            libraries.push(MusicLibrary { key, title });
        }
        libraries.sort_by_cached_key(|library| library.title.to_lowercase());
        Ok((libraries, catalog))
    }

    pub(super) fn check_music_access(&self, timeout_ms: u32) -> Result<(), Error> {
        let (libraries, _) = self.read_libraries(timeout_ms)?;
        if libraries.is_empty() {
            return Err(Error::NotPermitted("no accessible Plex music libraries".into()));
        }
        Ok(())
    }

    pub(in crate::server) fn library_catalog(&self) -> Result<(Vec<MusicLibrary>, Option<CatalogState>), Error> {
        self.verify_machine()?;
        self.read_libraries(subsonic::TIMEOUT_MS)
    }

    pub(in crate::server) fn catalog_state(&self) -> Result<CatalogState, Error> {
        self.library_catalog()?.1.ok_or_else(|| bad("the selected Plex music library is unavailable"))
    }

    pub(in crate::server) fn albums(&self, section: &str, size: u32, offset: u32) -> Result<Vec<ListedAlbum>, Error> {
        if !is_id(section) {
            return Err(bad("invalid Plex music library identifier"));
        }
        let path = format!("/library/sections/{section}/all?type=9");
        let mut albums = Vec::new();
        self.pages(&path, offset, size, None, |row| {
            let id = self.scoped(&rating_key(row, "album")?);
            let album = RemoteAlbum {
                id,
                title: title(row).unwrap_or_default(),
                artist: string(row, "parentTitle").unwrap_or_default(),
                date: number(&row["year"]).map(|y| y.to_string()),
                release_date: string(row, "originallyAvailableAt"),
                created: millis(row),
                genres: genres(row),
                label: string(row, "studio"),
                cover_art: self.thumb(row),
                ..RemoteAlbum::default()
            };
            albums.push(ListedAlbum { album, song_count: integer(&row["leafCount"]).unwrap_or_default() });
            Ok(())
        })?;
        Ok(albums)
    }

    pub(in crate::server) fn album(&self, listed: &ListedAlbum) -> Result<RemoteAlbum, Error> {
        let key = self.key(&listed.album.id)?;
        if !is_id(key) {
            return Err(bad("invalid Plex album identifier"));
        }
        let mut album = listed.album.clone();
        let path = format!("/library/metadata/{key}/children");
        let mut seen = HashSet::new();
        self.pages(&path, 0, u32::MAX, Some(listed.song_count), |row| {
            let id = self.scoped(&rating_key(row, "track")?);
            if !seen.insert(id.clone()) {
                return Err(bad("duplicate track in Plex album listing"));
            }
            let (media, part) = media_part(row).unwrap_or((&Value::Null, &Value::Null));
            if album.title.is_empty() && let Some(parent) = string(row, "parentTitle") {
                album.title = parent;
            }
            let artist = string(row, "originalTitle").or_else(|| string(row, "grandparentTitle")).unwrap_or_else(|| album.artist.clone());
            let track = integer(&row["index"]);
            let path = string(part, "file");
            let title = title(row).or_else(|| file_title(path.as_deref()?))
                .unwrap_or_else(|| track.map_or_else(|| "Untitled track".into(), |n| format!("Track {n}")));
            let song = RemoteSong {
                id, title, artist,
                track, disc: integer(&row["parentIndex"]),
                duration: number(&row["duration"]).unwrap_or_default() as f64 / 1000.0,
                date: number(&row["year"]).map(|y| y.to_string()),
                genres: genres(row), created: millis(row), cover_art: self.thumb(row),
                suffix: string(part, "container").or_else(|| string(media, "container")),
                size: integer(&part["size"]).unwrap_or_default(), bitrate: integer(&media["bitrate"]), channels: integer(&media["audioChannels"]),
                path,
                ..RemoteSong::default()
            };
            album.songs.push(song);
            Ok(())
        })?;
        if album.title.is_empty() {
            album.title = folder_title(&album.songs).unwrap_or_else(|| "Untitled album".into());
        }
        Ok(album)
    }

    fn thumb(&self, item: &Value) -> Option<String> {
        item["thumb"].as_str().filter(|s| media_path(s, "/library/metadata/")).map(|key| self.scoped(key))
    }

    // Fill the caller's requested window even if Plex caps a response below our page size.
    // A server ignoring offsets or returning fewer rows than it counts cannot authorize removal.
    fn pages(&self, path: &str, mut offset: u32, mut remaining: u32, expected: Option<u32>, mut visit: impl FnMut(&Value) -> Result<(), Error>) -> Result<(), Error> {
        const PAGE_SIZE: u32 = 500;
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut previous_total = None;
        while remaining > 0 {
            let ask = remaining.min(PAGE_SIZE);
            let url = format!("{path}{sep}X-Plex-Container-Start={offset}&X-Plex-Container-Size={ask}");
            let container = self.json(&url)?;
            let rows = items(&container, "Metadata")?;
            let count = u32::try_from(rows.len()).map_err(|_| bad("invalid Plex page size"))?;
            let end = offset.checked_add(count).ok_or_else(|| bad("invalid Plex page range"))?;
            let total = integer::<u32>(&container["totalSize"]);
            let reported = integer::<u32>(&container["offset"]);
            if count > ask || reported.is_some_and(|n| n != offset) || (offset > 0 && count > 0 && reported.is_none())
                || (previous_total.is_some() && previous_total != total)
                || total.is_some_and(|t| count > 0 && end > t || count == 0 && offset < t) {
                return Err(bad("inconsistent Plex library pagination"));
            }
            for item in rows {
                visit(item)?;
            }
            // Album children may omit totalSize. A short page that accounts for the album's
            // track count is complete. Otherwise probe the next page in case PMS capped it.
            let at_end = match total {
                Some(total) => end >= total,
                None => count < ask && expected == Some(end),
            };
            if count == 0 || at_end {
                break;
            }
            remaining -= count;
            previous_total = total;
            offset = end;
        }
        Ok(())
    }
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn integer<T: TryFrom<u64>>(value: &Value) -> Option<T> {
    number(value)?.try_into().ok()
}

fn string(row: &Value, field: &str) -> Option<String> {
    row[field].as_str().filter(|v| !v.trim().is_empty()).map(str::to_owned)
}

fn millis(row: &Value) -> Option<i64> {
    integer::<i64>(&row["addedAt"])?.checked_mul(1000)
}

fn genres(row: &Value) -> Vec<String> {
    row["Genre"].as_array().into_iter().flatten().filter_map(|g| string(g, "tag")).collect()
}

fn title(row: &Value) -> Option<String> {
    string(row, "title").or_else(|| string(row, "titleSort"))
}

fn file_title(file: &str) -> Option<String> {
    let filename = file.rsplit(['/', '\\']).next()?;
    let stem = Path::new(filename).file_stem()?.to_str()?.trim();
    (!stem.is_empty()).then(|| library::names::from_file_name(stem).title)
}

// Match the folder scanner's treatment of missing album tags, including disc subfolders.
// A Plex file path uses the server OS's separators, independently of the player's OS.
fn folder_title(songs: &[RemoteSong]) -> Option<String> {
    let mut folders = songs.iter().filter_map(|s| s.path.as_deref()).map(|p| library::scan::group_folder(Path::new(&p.replace('\\', "/"))));
    let folder = folders.next()?;
    if folders.any(|other| other != folder) {
        return None;
    }
    folder.file_name()?.to_str().map(str::to_owned)
}

fn identifier(value: &Value) -> Result<String, Error> {
    value.as_str().filter(|v| is_id(v)).map(str::to_owned).or_else(|| value.as_u64().map(|n| n.to_string())).ok_or_else(|| bad("missing or invalid Plex identifier"))
}

pub(super) fn rating_key(item: &Value, expected: &str) -> Result<String, Error> {
    if item["type"].as_str() != Some(expected) {
        return Err(bad("unexpected Plex item type"));
    }
    identifier(&item["ratingKey"])
}

pub(super) fn items<'a>(container: &'a Value, field: &str) -> Result<&'a [Value], Error> {
    let size = integer::<usize>(&container["size"]).ok_or_else(|| bad("missing Plex listing size"))?;
    match container.get(field) {
        Some(Value::Array(v)) if v.len() == size => Ok(v.as_slice()),
        None if size == 0 => Ok(&[]),
        _ => Err(bad("incomplete Plex library listing")),
    }
}

/// Direct play needs one continuous audio file; a track may have several media versions.
pub(super) fn media_part(track: &Value) -> Option<(&Value, &Value)> {
    track["Media"].as_array()?.iter().find_map(|media| {
        let [part] = media["Part"].as_array()?.as_slice() else { return None };
        part["key"].as_str().filter(|p| media_path(p, "/library/parts/")).map(|_| (media, part))
    })
}
