//! Read-only access to Plex Media Server. A rating key identifies a library item, while
//! a Part key identifies the current file. Resolve the latter at playback, including after
//! a restart or a replacement of the track's media on the server.

mod catalog;
pub(crate) mod cloud;

use http_client::audio::{self, StreamError, StreamSink, Streamed};
use http_client::{BinaryResponse, Request};
use serde_json::Value;
use subsonic::Error;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    client_id: String,
    machine_id: String,
    library_key: Option<String>,
    pub stream_timeout_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MusicLibrary {
    pub key: String,
    pub title: String,
}

pub struct ServerInfo {
    pub machine_id: String,
    pub version: String,
}

pub fn sign_in(url: &str, token: &str) -> Result<(Client, ServerInfo), Error> {
    let mut client = Client::connection(url, token, &subsonic::new_salt())?;
    let info = client.identity()?;
    client.machine_id.clone_from(&info.machine_id);
    // /identity is public. Test the token against a protected resource before saving it.
    client.check_music_access(subsonic::TIMEOUT_MS)?;
    Ok((client, info))
}

impl Client {
    fn connection(url: &str, token: &str, client_id: &str) -> Result<Self, Error> {
        let token = token.trim();
        if !valid_token(token) {
            return Err(status_error(401));
        }
        if !is_id(client_id) {
            return Err(bad("invalid Plex client identifier"));
        }
        Ok(Self {
            base: http_client::normalize_base_url(url, &["/web/index.html", "/web"]).ok_or(Error::BadUrl)?,
            token: token.into(),
            client_id: client_id.into(),
            machine_id: String::new(),
            library_key: None,
            stream_timeout_ms: subsonic::STREAM_TIMEOUT_MS,
        })
    }

    pub fn new(url: &str, token: &str, machine_id: &str, client_id: &str) -> Result<Self, Error> {
        let mut client = Self::connection(url, token, client_id)?;
        if !is_id(machine_id) {
            return Err(bad("invalid Plex machine identifier"));
        }
        client.machine_id = machine_id.into();
        Ok(client)
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn select_library(&mut self, key: Option<&str>) -> Result<(), Error> {
        if key.is_some_and(|k| !is_id(k)) {
            return Err(bad("invalid Plex music library identifier"));
        }
        self.library_key = key.map(str::to_owned);
        Ok(())
    }

    pub fn same_credentials(&self, other: &Self) -> bool {
        self.base == other.base && self.token == other.token && self.machine_id == other.machine_id && self.client_id == other.client_id
    }

    // At a manually entered or already verified address, proxies may require credentials
    // even for Plex's public identity endpoint.
    fn identity(&self) -> Result<ServerInfo, Error> {
        server_info(&self.json("/identity")?)
    }

    fn public_identity(&self, timeout_ms: u32) -> Result<ServerInfo, Error> {
        server_info(&self.json_with_timeout("/identity", timeout_ms, None)?)
    }

    fn verify_machine(&self) -> Result<(), Error> {
        if self.identity()?.machine_id != self.machine_id {
            return Err(bad("a different Plex server now answers at this address"));
        }
        Ok(())
    }

    fn request<T>(&self, path: &str, accept: &str, timeout_ms: u32, token: Option<&str>, send: impl FnOnce(&Request) -> T) -> T {
        let url = format!("{}{path}", self.base);
        with_headers(accept, &self.client_id, token, |headers| {
            send(&Request { method: "GET", url: &url, headers, body: &[], user_agent: subsonic::CLIENT_NAME, timeout_ms })
        })
    }

    fn get(&self, path: &str, accept: &str, timeout_ms: u32, token: Option<&str>) -> Result<BinaryResponse, Error> {
        let reply = self.request(path, accept, timeout_ms, token, http_client::request_bytes_same_server).map_err(|e| Error::Network(e.to_string()))?;
        if reply.status != 200 {
            return Err(status_error(reply.status));
        }
        Ok(reply)
    }

    fn json(&self, path: &str) -> Result<Value, Error> {
        self.json_with_timeout(path, subsonic::TIMEOUT_MS, Some(&self.token))
    }

    fn json_with_timeout(&self, path: &str, timeout_ms: u32, token: Option<&str>) -> Result<Value, Error> {
        let reply = self.get(path, "Accept: application/json", timeout_ms, token)?;
        let mut value: Value = serde_json::from_slice(&reply.body).map_err(|_| bad("expected JSON from Plex Media Server"))?;
        value.get_mut("MediaContainer").filter(|c| c.is_object()).map(Value::take).ok_or_else(|| bad("missing Plex media container"))
    }

    // The database and stream cache keep these keys across sign-outs and address changes.
    fn scoped(&self, key: &str) -> String {
        format!("plex:{}:{key}", self.machine_id)
    }

    fn key<'a>(&self, id: &'a str) -> Result<&'a str, Error> {
        let pair = id.strip_prefix("plex:").and_then(|s| s.split_once(':'));
        match pair {
            Some((machine, key)) if machine == self.machine_id => Ok(key),
            _ => Err(Error::NotFound("item belongs to a different Plex server".into())),
        }
    }

    pub fn cover(&self, id: &str, size: u32) -> Result<Vec<u8>, Error> {
        let path = self.key(id)?;
        if !media_path(path, "/library/metadata/") {
            return Err(bad("invalid Plex thumbnail path"));
        }
        let url = format!("/photo/:/transcode?width={size}&height={size}&url={}", http_client::encode(path));
        let reply = self.get(&url, "Accept: image/*", subsonic::TIMEOUT_MS, Some(&self.token))?;
        if reply.body.is_empty() || reply.content_type.as_deref().is_some_and(|t| t.starts_with("text/") || t.contains("json") || t.contains("xml")) {
            return Err(bad("expected a Plex cover image"));
        }
        Ok(reply.body)
    }

    pub fn stream_quality(&self, id: &str, bitrate: Option<u32>, from: u64, validator: Option<&str>, sink: &mut dyn StreamSink) -> Result<Streamed, Error> {
        let key = self.key(id)?;
        if !is_id(key) {
            return Err(bad("invalid Plex track identifier"));
        }
        self.verify_machine()?;
        let (path, from, validator) = match bitrate {
            // Each live encode starts a fresh byte stream, including after an interrupted download.
            Some(kbps) => (mp3_path(key, kbps), 0, None),
            None => (self.part_path(key)?, from, validator),
        };
        self.request(&path, "Accept: */*", self.stream_timeout_ms, Some(&self.token), |req| audio::download(req, from, validator, sink)).map_err(|e| match e {
            StreamError::Response { status, .. } if (200..300).contains(&status) => bad("expected audio from Plex"),
            StreamError::Response { status, .. } => status_error(status),
            StreamError::BadRange => bad("invalid Content-Range"),
            StreamError::Network(e) => Error::Network(e.to_string()),
        })
    }

    fn part_path(&self, key: &str) -> Result<String, Error> {
        let container = self.json(&format!("/library/metadata/{key}"))?;
        let tracks = catalog::items(&container, "Metadata")?;
        let [track] = tracks else { return Err(bad("expected one Plex track")); };
        if catalog::rating_key(track, "track")? != key {
            return Err(bad("Plex returned a different track"));
        }
        let (_, part) = catalog::media_part(track).ok_or_else(|| Error::NotFound("no playable Plex media part".into()))?;
        let path = part["key"].as_str().filter(|p| media_path(p, "/library/parts/")).ok_or_else(|| bad("invalid Plex media part path"))?;
        Ok(path.into())
    }
}

fn mp3_path(key: &str, kbps: u32) -> String {
    // PMS profile names are case-sensitive. Override the Generic profile's streaming
    // target so every desktop OS receives MP3 bytes over HTTP.
    let profile = http_client::encode("add-transcode-target(type=musicProfile&context=streaming&protocol=http&container=mp3&audioCodec=mp3&replace=true)");
    let track = http_client::encode(&format!("/library/metadata/{key}"));
    let session = subsonic::new_salt();
    format!("/music/:/transcode/universal/start.mp3?path={track}&protocol=http&musicBitrate={kbps}&directPlay=0&directStream=0&directStreamAudio=0&X-Plex-Client-Profile-Name=Generic&X-Plex-Client-Profile-Extra={profile}&session={session}")
}

fn server_info(info: &Value) -> Result<ServerInfo, Error> {
    let id = info["machineIdentifier"].as_str().filter(|s| is_id(s)).ok_or_else(|| bad("missing Plex machine identifier"))?;
    Ok(ServerInfo { machine_id: id.into(), version: info["version"].as_str().unwrap_or_default().into() })
}

fn status_error(status: u16) -> Error {
    match status {
        401 => Error::WrongCredentials { code: 401, message: "Plex refused the access token".into() },
        403 => Error::NotPermitted("Plex denied access".into()),
        404 | 410 => Error::NotFound("Plex item unavailable".into()),
        _ => Error::Server { code: None, http_status: Some(status), message: String::new() },
    }
}

fn bad(message: &'static str) -> Error {
    Error::BadResponse(message.into())
}

/// The same device identity appears in account authorization, discovery and PMS requests.
fn with_headers<T>(accept: &str, client_id: &str, token: Option<&str>, send: impl FnOnce(&[&str]) -> T) -> T {
    let id = format!("X-Plex-Client-Identifier: {client_id}");
    let platform = format!("X-Plex-Platform: {}", std::env::consts::OS);
    let token = token.map(|t| format!("X-Plex-Token: {t}"));
    let mut headers = vec![accept, "X-Plex-Product: Marquee Player", concat!("X-Plex-Version: ", env!("APP_VERSION")), &platform, &id];
    headers.extend(token.as_deref());
    send(&headers)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic())
}

fn is_id(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// Only server-local library resources can receive the session headers. Never retain
// queries with credentials in the catalog, or pass a URL supplied by metadata to Plex.
fn media_path(path: &str, prefix: &str) -> bool {
    if !path.starts_with(prefix) || path.contains(['?', '#', '\\']) || path.bytes().any(|b| b.is_ascii_control()) {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    !lower.contains("%2f") && !lower.contains("%5c") && !lower.split('/').any(|s| matches!(s.replace("%2e", ".").as_str(), "." | ".."))
}

#[cfg(test)]
mod tests;
