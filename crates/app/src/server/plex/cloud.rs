//! Native Plex sign-in. Only the PIN code goes to the browser. The account token lives for
//! one resources request, and only the chosen server's token is handed to account storage.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use http_client::Request;
use serde_json::Value;
use subsonic::Error;

use super::{Client, ServerInfo, is_id, valid_token, with_headers};

const API: &str = "https://plex.tv/api/v2";
const RESOURCES: &str = "/resources?includeHttps=1&includeRelay=1&includeIPv6=1";
const EXPIRED: &str = "This Plex sign-in expired. Start sign-in again.";
const BAD_REPLY: &str = "Plex returned an unreadable sign-in response.";

/// PINs, server access tokens and their request URLs must stay out of logs.
pub struct Pin {
    api: String,
    id: u64,
    code: String,
    client_id: String,
    expires: Instant,
}

impl Pin {
    pub fn start(client_id: &str) -> Result<Self, &'static str> {
        Self::start_at(API, client_id)
    }

    fn start_at(api: &str, client_id: &str) -> Result<Self, &'static str> {
        if !is_id(client_id) {
            return Err("Invalid Plex client identifier.");
        }
        let created = Instant::now();
        let data = request(api, client_id, "POST", "/pins?strong=true", None)?;
        let id = data["id"].as_u64().filter(|n| *n > 0).ok_or(BAD_REPLY)?;
        let code = data["code"].as_str().filter(|s| !s.is_empty()).ok_or(BAD_REPLY)?;
        // Plex also reports expiration on poll. Limit the local wait even if the API stops
        // supplying its lifetime, or a server clock is far from this computer's.
        let seconds = data["expiresIn"].as_u64().unwrap_or(1800).min(86400);
        if seconds == 0 {
            return Err(EXPIRED);
        }
        Ok(Self { api: api.into(), id, code: code.into(), client_id: client_id.into(), expires: created + Duration::from_secs(seconds) })
    }

    pub fn browser_url(&self) -> String {
        format!("https://app.plex.tv/auth#?clientID={}&code={}&context%5Bdevice%5D%5Bproduct%5D=Marquee%20Player",
            http_client::encode(&self.client_id), http_client::encode(&self.code))
    }

    /// Called once per second by the sign-in worker until the user authorizes or cancels.
    pub fn poll(&self) -> Result<Option<Vec<Resource>>, &'static str> {
        if Instant::now() >= self.expires {
            return Err(EXPIRED);
        }
        let pin = request(&self.api, &self.client_id, "GET", &format!("/pins/{}", self.id), None)?;
        if pin["expired"] == true || pin["expiresIn"].as_u64() == Some(0) {
            return Err(EXPIRED);
        }
        if pin["id"].as_u64() != Some(self.id) {
            return Err(BAD_REPLY);
        }
        let token = match pin.get("authToken") {
            None | Some(Value::Null) => return Ok(None),
            Some(v) => v.as_str().filter(|s| valid_token(s)).ok_or(BAD_REPLY)?,
        };
        let data = request(&self.api, &self.client_id, "GET", RESOURCES, Some(token))?;
        Ok(Some(resources(&data, &self.client_id)?))
    }
}

fn request(api: &str, client_id: &str, method: &str, path: &str, token: Option<&str>) -> Result<Value, &'static str> {
    let url = format!("{api}{path}");
    let reply = with_headers("Accept: application/json", client_id, token, |headers| {
        http_client::request_bytes_same_server(&Request { method, url: &url, headers, body: &[], user_agent: subsonic::CLIENT_NAME, timeout_ms: 8000 })
    }).map_err(|_| "Could not reach Plex. Check your connection and try again.")?;
    match reply.status {
        200..300 => serde_json::from_slice(&reply.body).map_err(|_| BAD_REPLY),
        404 | 410 if path.starts_with("/pins/") => Err(EXPIRED),
        401 | 403 => Err("Plex refused this sign-in. Start sign-in again."),
        429 => Err("Plex is limiting sign-in requests. Try again shortly."),
        _ => Err("Plex sign-in is unavailable. Try again shortly."),
    }
}

/// One owned or shared server offered by the account. Its connections have already been
/// ordered, and the account token is never a fallback for a missing server token.
#[derive(Clone)]
pub struct Resource {
    pub name: String,
    machine_id: String,
    token: String,
    client_id: String,
    urls: Vec<String>,
}

fn resources(data: &Value, client_id: &str) -> Result<Vec<Resource>, &'static str> {
    let mut servers = Vec::new();
    let mut seen = HashSet::new();
    for entry in data.as_array().ok_or(BAD_REPLY)? {
        if !entry["provides"].as_str().is_some_and(|s| s.split(',').any(|p| p.trim() == "server")) {
            continue;
        }
        let Some(machine) = entry["clientIdentifier"].as_str().filter(|s| is_id(s)) else { continue };
        let Some(token) = entry["accessToken"].as_str().filter(|s| valid_token(s)) else { continue };
        let urls = connection_urls(entry);
        if !urls.is_empty() && seen.insert(machine) {
            let name = entry["name"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or("Plex Media Server");
            let owner = entry["sourceTitle"].as_str().filter(|s| !s.trim().is_empty());
            servers.push((Resource { name: name.into(), machine_id: machine.into(), client_id: client_id.into(), token: token.into(), urls }, owner));
        }
    }
    servers.sort_by_cached_key(|(s, _)| s.name.to_lowercase());
    // Personal and shared servers often both have the default name. Show the owner when
    // it distinguishes them; fall back to the machine identifier for the ambiguous ones.
    for group in servers.chunk_by_mut(|(a, _), (b, _)| a.name.eq_ignore_ascii_case(&b.name)) {
        if group.len() > 1 {
            let mut owner_counts = std::collections::HashMap::new();
            for (_, owner) in group.iter() {
                *owner_counts.entry(*owner).or_insert(0) += 1;
            }
            for (server, owner) in group {
                let hint = owner.filter(|_| owner_counts.get(owner) == Some(&1)).unwrap_or(&server.machine_id);
                server.name = format!("{} ({hint})", server.name);
            }
        }
    }
    Ok(servers.into_iter().map(|(server, _)| server).collect())
}

fn connection_urls(entry: &Value) -> Vec<String> {
    let mut routes = Vec::new();
    for route in entry["connections"].as_array().into_iter().flatten() {
        let Some(uri) = route["uri"].as_str().filter(|u| !u.contains(['?', '#', '\\']) && u.contains("://")) else { continue };
        let Some(url) = http_client::normalize_base_url(uri, &[]) else { continue };
        let local = route["local"] == true;
        let relay = route["relay"] == true;
        let secure = url.starts_with("https://");
        if !secure && (!local || relay || entry["httpsRequired"] == true || !lan_address(&url)) {
            continue;
        }
        routes.push(((!secure, relay, !local), url));
    }
    routes.sort_by_key(|(rank, _)| *rank);
    let mut unique = HashSet::new();
    routes.into_iter().map(|(_, url)| url).filter(|url| unique.insert(url.clone())).collect()
}

// A shared server's "local" address is local to its owner. For an automatic, unencrypted
// fallback, require an actual local IP as well as the flag from plex.tv.
fn lan_address(url: &str) -> bool {
    let authority = url.strip_prefix("http://").unwrap_or("").split('/').next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    };
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        Ok(IpAddr::V6(v6)) => v6.is_unique_local() || v6.is_loopback() || v6.is_unicast_link_local(),
        Err(_) => host == "localhost",
    }
}

impl Resource {
    pub fn connect(&self, cancelled: &AtomicBool) -> Result<(Client, ServerInfo, String), &'static str> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut reason = "Could not reach the selected Plex server. Check that it is online and try again.";
        for (index, url) in self.urls.iter().enumerate() {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Plex sign-in was cancelled.");
            }
            let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
            if remaining == 0 {
                break;
            }
            // Leave time to try remote routes when a server advertises stale LAN addresses.
            // The HTTP client caches an agent per timeout, so use a few fixed budgets.
            let probe_ms = if remaining / (self.urls.len() - index) as u64 >= 3 { 3000 } else { 1000 };
            let client = Client::new(url, &self.token, &self.machine_id, &self.client_id).map_err(|_| BAD_REPLY)?;
            let Ok(info) = client.public_identity(probe_ms) else { continue };
            if info.machine_id != self.machine_id {
                continue;
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err("Plex sign-in was cancelled.");
            }
            let read_ms = match deadline.saturating_duration_since(Instant::now()).as_secs() {
                0 => break,
                1..=2 => 1000,
                3..=7 => 3000,
                _ => 8000,
            };
            match client.check_music_access(read_ms) {
                Ok(_) => return Ok((client, info, self.token.clone())),
                Err(Error::NotPermitted(message)) if message == "no accessible Plex music libraries" => return Err("This Plex server has no music library you can access."),
                Err(Error::WrongCredentials { .. } | Error::NotPermitted(_)) => reason = "The selected Plex server denied access to its library.",
                Err(_) => reason = "The selected Plex server could not list its music libraries.",
            }
        }
        Err(reason)
    }
}

#[cfg(test)]
mod tests;
