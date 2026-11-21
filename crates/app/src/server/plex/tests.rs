use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::ops::ControlFlow;

use serde_json::{Value, json};

use super::*;

const TOKEN: &str = "mock-music-token";
const MACHINE: &str = "server-1";
const PART: &str = "/library/parts/81/1708080808/file.flac";
const MP3: &str = "/music/:/transcode/universal/start.mp3";

pub(super) struct Response {
    method: &'static str,
    token: Option<&'static str>,
    path: &'static str,
    status: u16,
    headers: &'static str,
    data: Vec<u8>,
}

pub(super) fn reply(method: &'static str, path: &'static str, token: Option<&'static str>, status: u16, data: Value) -> Response {
    Response { method, path, token, status, headers: "Content-Type: application/json\r\n", data: data.to_string().into_bytes() }
}

fn container(path: &'static str, value: Value) -> Response {
    reply("GET", path, Some(TOKEN), 200, json!({"MediaContainer": value}))
}

pub(super) fn server(responses: Vec<Response>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for reply in responses {
            let (mut conn, _) = listener.accept().unwrap();
            let request: Vec<String> = BufReader::new(&conn).lines().map(Result::unwrap).take_while(|s| !s.is_empty()).collect();
            if reply.path == MP3 {
                let path = request[0].strip_prefix("GET ").unwrap().strip_suffix(" HTTP/1.1").unwrap();
                let (url, session) = path.rsplit_once("&session=").unwrap();
                assert_eq!(url, concat!("/music/:/transcode/universal/start.mp3?path=%2Flibrary%2Fmetadata%2F20&protocol=http&musicBitrate=128",
                    "&directPlay=0&directStream=0&directStreamAudio=0&X-Plex-Client-Profile-Name=Generic",
                    "&X-Plex-Client-Profile-Extra=add-transcode-target%28type%3DmusicProfile%26context%3Dstreaming%26protocol%3Dhttp",
                    "%26container%3Dmp3%26audioCodec%3Dmp3%26replace%3Dtrue%29"));
                assert!(session.len() == 32 && session.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(!request.iter().any(|h| h.to_ascii_lowercase().starts_with("range:") || h.to_ascii_lowercase().starts_with("if-range:")));
            } else {
                assert_eq!(request[0], format!("{} {} HTTP/1.1", reply.method, reply.path));
            }
            assert!(!request[0].contains(TOKEN));
            let token = request.iter().filter_map(|h| h.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case("X-Plex-Token")).map(|(_, v)| v.trim());
            assert_eq!(token, reply.token);
            assert!(request.iter().any(|h| h.to_ascii_lowercase().starts_with("x-plex-client-identifier: ")));
            if reply.path == PART {
                assert!(request.iter().any(|h| h.eq_ignore_ascii_case("Range: bytes=2-")));
                assert!(request.iter().any(|h| h.eq_ignore_ascii_case("If-Range: \"abc\"")));
            }
            write!(conn, "HTTP/1.1 {} Test\r\n{}", reply.status, reply.headers).unwrap();
            if reply.path != MP3 {
                write!(conn, "Content-Length: {}\r\n", reply.data.len()).unwrap();
            }
            write!(conn, "Connection: close\r\n\r\n").unwrap();
            conn.write_all(&reply.data).unwrap();
        }
    });
    (url, thread)
}

#[test]
fn capped_pages_and_playback_use_stable_ids_and_current_media_parts() {
    let album = json!({"type":"album","ratingKey":"10","title":"A","parentTitle":"B","leafCount":1,"thumb":"/library/metadata/10/thumb/123"});
    let track = json!({"type":"track","ratingKey":"20","title":"C","index":2,"parentIndex":1,"originalTitle":"Guest Artist","duration":12125,
        "Media":[{"audioChannels":2,"bitrate":"750","Part":[{"size":"4","file":"/music/A/C.flac","container":"flac","key":PART}]}]});
    let (url, join) = server(vec![
        container("/identity", json!({"machineIdentifier":MACHINE,"version":"1.41"})),
        container("/library/sections", json!({"size":2,"Directory":[{"type":"movie","key":"2"},{"type":"artist","key":"3"}]})),
        container("/library/sections/3/all?type=9&X-Plex-Container-Start=0&X-Plex-Container-Size=500", json!({"size":1,"offset":0,"totalSize":2,"Metadata":[album]})),
        container("/library/sections/3/all?type=9&X-Plex-Container-Start=1&X-Plex-Container-Size=499", json!({"size":1,"offset":1,"totalSize":2,"Metadata":[{"type":"album","ratingKey":11,"title":"D"}]})),
        container("/library/metadata/10/children?X-Plex-Container-Start=0&X-Plex-Container-Size=500", json!({"size":1,"Metadata":[track]})),
        container("/identity", json!({"machineIdentifier":MACHINE})),
        container("/library/metadata/20", json!({"size":1,"Metadata":[track]})),
        Response { method: "GET", token: Some(TOKEN), path: PART, status: 206, headers: "Content-Type: audio/flac\r\nContent-Range: bytes 2-3/4\r\nETag: \"abc\"\r\n", data: b"34".to_vec() },
    ]);
    let (client, info) = sign_in(&url, TOKEN).unwrap();
    assert_eq!(info.machine_id, MACHINE);
    let albums = client.albums("3", 500, 0).unwrap();
    assert_eq!(albums.len(), 2);
    let album = client.album(&albums[0]).unwrap();
    assert_eq!(album.cover_art.as_deref(), Some("plex:server-1:/library/metadata/10/thumb/123"));
    let song = &album.songs[0];
    assert_eq!((&*song.id, &*song.artist, song.disc, song.track, song.duration), ("plex:server-1:20", "Guest Artist", Some(1), Some(2), 12.125));
    assert_eq!((song.path.as_deref(), song.suffix.as_deref(), song.size, song.bitrate), (Some("/music/A/C.flac"), Some("flac"), 4, Some(750)));

    let restored = Client::new(&url, TOKEN, MACHINE, client.client_id()).unwrap();
    let mut sink = Sink(Vec::new());
    let head = restored.stream_quality(&song.id, None, 2, Some("\"abc\""), &mut sink).unwrap().head;
    assert!(head.range_honoured);
    assert_eq!((head.start, head.total), (2, Some(4)));
    assert_eq!(sink.0, b"34");
    join.join().unwrap();
}

struct Sink(Vec<u8>);
impl StreamSink for Sink {
    fn start(&mut self, _: &audio::StreamHead) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
    fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
        self.0.extend_from_slice(bytes);
        ControlFlow::Continue(())
    }
}

#[test]
fn capped_mp3_starts_a_fresh_progressive_session() {
    let (url, join) = server(vec![
        container("/identity", json!({"machineIdentifier":MACHINE})),
        Response { method: "GET", token: Some(TOKEN), path: MP3, status: 200, headers: "Content-Type: audio/mpeg\r\n", data: b"mp3".to_vec() },
    ]);
    let client = super::super::provider::Client::Plex(Client::new(&url, TOKEN, MACHINE, "mock").unwrap());
    let mut sink = Sink(Vec::new());
    let played = client.stream_quality("plex:server-1:20", Some(128), 731, Some("\"old\""), &mut sink).unwrap();
    assert_eq!((played.head.start, played.head.total, played.head.content_type.as_deref()), (0, None, Some("audio/mpeg")));
    assert_eq!(sink.0, b"mp3");
    join.join().unwrap();
}

#[test]
fn failed_auth_partial_listings_and_server_replacement_are_errors() {
    let mut refused = container("/library/sections", json!({}));
    refused.status = 401;
    let (url, join) = server(vec![
        container("/identity", json!({"machineIdentifier":MACHINE})),
        refused,
        container("/identity", json!({"machineIdentifier":MACHINE})),
        container("/library/sections", json!({"size":1,"Directory":[{"type":"movie","key":"2"}]})),
        container("/library/sections/3/all?type=9&X-Plex-Container-Start=0&X-Plex-Container-Size=500", json!({"size":0,"totalSize":5})),
        container("/identity", json!({"machineIdentifier":"different"})),
    ]);
    assert!(matches!(sign_in(&url, TOKEN), Err(Error::WrongCredentials { .. })));
    let Err(error) = sign_in(&url, TOKEN) else { panic!("a video-only account has no music libraries") };
    assert!(super::super::sign_in_words(&error, true).contains("music library"));
    let client = Client::new(&url, TOKEN, MACHINE, "mock").unwrap();
    assert!(client.albums("3", 500, 0).is_err());
    assert!(client.catalog_state().is_err());
    assert!(client.key("plex:different:20").is_err());
    for path in ["https://other/file", "//other/file", "/library/parts/%2E%2e/system", "/library/parts/file?X-Plex-Token=secret"] {
        assert!(!media_path(path, "/library/parts/"));
    }
    assert!(Client::new(&url, "token\r\nx-header: injected", MACHINE, "mock").is_err());
    join.join().unwrap();
}

#[test]
fn plex_web_addresses_keep_the_proxy_path() {
    let base = |url| Client::new(url, TOKEN, MACHINE, "mock").unwrap().base;
    assert_eq!(base("http://example.org:32400/web/"), "http://example.org:32400");
    assert_eq!(base("example.org/plex/web/index.html#!/media/42"), "https://example.org/plex");
    assert_eq!(base("https://example.org/rest"), "https://example.org/rest");
    assert_eq!(base("https://web"), "https://web");
}

#[test]
fn titleless_plex_albums_and_tracks_keep_their_ids_and_neighbours() {
    let album = json!({"type":"album","ratingKey":"10","leafCount":3,"parentTitle":"Artist"});
    let sibling = json!({"type":"album","ratingKey":"11","title":"Titled album","leafCount":1});
    let numbered = json!({"type":"track","ratingKey":"21","index":1,"Media":[{"Part":[{"key":PART,"file":r"D:\Music\A record\Disc 1\01 - Opening.flac"}]}]});
    let tagged = json!({"type":"track","ratingKey":"22","title":"Tagged","Media":[{"Part":[{"key":PART,"file":"D:/Music/A record/Disc 2/02.flac"}]}]});
    let hidden_file = json!({"type":"track","ratingKey":"23","index":3});
    let (url, join) = server(vec![
        container("/library/sections/7/all?type=9&X-Plex-Container-Start=0&X-Plex-Container-Size=500", json!({"size":2,"totalSize":2,"Metadata":[album,sibling]})),
        container("/library/metadata/10/children?X-Plex-Container-Start=0&X-Plex-Container-Size=500", json!({"size":3,"Metadata":[numbered,tagged,hidden_file]})),
    ]);
    let client = Client::new(&url, TOKEN, MACHINE, "mock").unwrap();
    let listed = client.albums("7", 500, 0).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].album.title, "Titled album");
    let read = client.album(&listed[0]).unwrap();
    assert_eq!((read.id.as_str(), read.title.as_str()), ("plex:server-1:10", "A record"));
    let tracks: Vec<_> = read.songs.iter().map(|song| (song.id.as_str(), song.title.as_str())).collect();
    assert_eq!(tracks, [("plex:server-1:21", "Opening"), ("plex:server-1:22", "Tagged"), ("plex:server-1:23", "Track 3")]);
    join.join().unwrap();
}

#[test]
fn music_library_selection_scopes_the_catalog_and_fails_closed_when_absent() {
    let sections = json!({"size":4,"Directory":[{"key":"2","title":"TV","type":"show"},
        {"key":"3","title":"Singles","type":"artist","refreshing":true}, {"key":"4","title":"Films","type":"movie"},
        {"key":"7","title":"Albums","type":"artist","contentChangedAt":"6"}]});
    let removed = json!({"size":1,"Directory":[{"key":"3","title":"Singles","type":"artist"}]});
    let empty = json!({"size":1,"Directory":[{"key":"2","title":"TV","type":"show"}]});
    let mut replies = Vec::new();
    for listing in [&sections, &sections, &removed, &removed, &empty] {
        replies.push(container("/identity", json!({"machineIdentifier":MACHINE})));
        replies.push(container("/library/sections", listing.clone()));
    }
    let (url, join) = server(replies);
    let mut client = Client::new(&url, TOKEN, MACHINE, "mock").unwrap();
    let (music, catalog) = client.library_catalog().unwrap();
    assert!(catalog.is_none());
    let names: Vec<_> = music.iter().map(|l| (&*l.key, &*l.title)).collect();
    assert_eq!(names, [("7", "Albums"), ("3", "Singles")]);
    client.select_library(Some("7")).unwrap();
    let selected = client.catalog_state().unwrap();
    assert_eq!(selected.sections, [Some("7".into())]);
    assert!(!selected.scanning, "a scan in another music library does not affect the selection");
    let (other, catalog) = client.library_catalog().unwrap();
    assert_eq!(other.len(), 1);
    assert!(catalog.is_none(), "losing access must not silently select a different library");
    assert!(client.catalog_state().is_err(), "a partial final listing cannot authorize pruning");
    let (none, catalog) = client.library_catalog().unwrap();
    assert!(none.is_empty() && catalog.is_none());
    join.join().unwrap();
}
