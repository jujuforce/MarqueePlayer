use super::*;
use serde_json::json;

use crate::server::plex::tests::{reply, server};

const ACCOUNT_TOKEN: &str = "account-token";
const SERVER_TOKEN: &str = "server-token";

#[test]
fn a_claimed_pin_discovers_servers_and_only_the_matching_machine_receives_its_token() {
    let identity = |machine| reply("GET", "/identity", None, 200, json!({"MediaContainer": {"machineIdentifier":machine}}));
    let (wrong_url, wrong) = server(vec![identity("someone-else")]);
    let (music_url, music) = server(vec![
        identity("music-id"),
        reply("GET", "/library/sections", Some(SERVER_TOKEN), 200, json!({"MediaContainer":{"size":1,"Directory":[{"key":"1","type":"artist"}]}})),
    ]);
    let (api, cloud) = server(vec![
        reply("POST", "/pins?strong=true", None, 201, json!({"id":71,"code":"one+two","expiresIn":1800})),
        reply("GET", "/pins/71", None, 200, json!({"id":71,"authToken":null})),
        reply("GET", "/pins/71", None, 200, json!({"id":71,"authToken":ACCOUNT_TOKEN})),
        reply("GET", RESOURCES, Some(ACCOUNT_TOKEN), 200, json!([{
            "name":"Music","provides":"server","clientIdentifier":"music-id","accessToken":SERVER_TOKEN,
            "connections":[{"uri":wrong_url,"local":true},{"uri":music_url,"local":true}]
        }])),
    ]);
    let pin = Pin::start_at(&api, "desktop").unwrap();
    assert_eq!(pin.browser_url(), "https://app.plex.tv/auth#?clientID=desktop&code=one%2Btwo&context%5Bdevice%5D%5Bproduct%5D=Marquee%20Player");
    assert!(pin.poll().unwrap().is_none());
    let servers = pin.poll().unwrap().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "Music");
    assert!(servers[0].connect(&AtomicBool::new(true)).is_err());
    let (client, info, sealed_secret) = servers[0].connect(&AtomicBool::new(false)).unwrap();
    assert_eq!((client.base_url(), client.client_id(), info.machine_id.as_str(), sealed_secret.as_str()), (music_url.as_str(), "desktop", "music-id", SERVER_TOKEN));
    for handle in [cloud, wrong, music] {
        handle.join().unwrap();
    }
}

#[test]
fn expires_and_fails_without_exposing_response_content() {
    let (api, cloud) = server(vec![
        reply("POST", "/pins?strong=true", None, 201, json!({"id":71,"code":"abcdef","expiresIn":30})),
        reply("GET", "/pins/71", None, 404, json!({"errors":[{"message":ACCOUNT_TOKEN}]})),
    ]);
    let mut pin = Pin::start_at(&api, "desktop").unwrap();
    assert!(matches!(pin.poll(), Err(EXPIRED)));
    pin.expires = Instant::now();
    assert!(matches!(pin.poll(), Err(EXPIRED)));
    cloud.join().unwrap();
}

#[test]
fn resources_prefer_direct_tls_and_respect_the_servers_secure_connection_setting() {
    let mut device = json!({"clientIdentifier":"a","name":"Plex","accessToken":SERVER_TOKEN,"provides":"server,player",
        "connections":[{"uri":"http://public.example","local":true},{"uri":"http://192.168.0.1","local":true},{"uri":"https://relay.example","relay":true},
                       {"uri":"https://remote.example"},{"uri":"https://remote.example"},{"uri":"https://local.plex.direct","local":true}]});
    let first = device.clone();
    device["clientIdentifier"] = "b".into();
    device["sourceTitle"] = "Alex".into();
    device["httpsRequired"] = true.into();
    let servers = resources(&json!([first, device, {"provides":"player"}, {"provides":"server","accessToken":null,"clientIdentifier":"no-token"}]), "desktop").unwrap();
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0].name, "Plex (a)");
    assert_eq!(servers[0].urls, ["https://local.plex.direct","https://remote.example","https://relay.example","http://192.168.0.1"]);
    assert_eq!(servers[1].name, "Plex (Alex)");
    assert_eq!(servers[1].urls.len(), 3);
    assert!(servers[1].urls.iter().all(|u| u.starts_with("https://")));
}
