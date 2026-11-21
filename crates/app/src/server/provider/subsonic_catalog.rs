//! Map Subsonic metadata and favourite timestamps onto the shared library model.

use library::remote::{RemoteAlbum, RemoteArtist, RemoteSong};

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
fn iso_ms(s: &str) -> Option<i64> {
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
pub(super) fn album_of(a: &subsonic::Album) -> RemoteAlbum {
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
}
