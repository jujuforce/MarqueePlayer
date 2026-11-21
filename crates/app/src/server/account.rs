//! The desktop Server sign-in. Both providers use the original `navidrome.json` path and
//! platform secret storage. Existing Subsonic files open without migration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use subsonic::{Credentials, Error};

use super::{plex, provider::Client};

/// The file's name, in the data folder.
pub const FILE: &str = "navidrome.json";

pub fn path(data: &Path) -> PathBuf {
    data.join(FILE)
}

/// What the server said it is at sign-in (nothing secret).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct About {
    /// "navidrome"; empty for a server that does not say (plain Subsonic).
    #[serde(default)]
    pub kind: String,
    /// "0.53.3 (13af8ed4)".
    #[serde(default)]
    pub version: String,
    /// The Subsonic API version ("1.16.1").
    #[serde(default)]
    pub api: String,
}

impl About {
    pub fn of(info: &subsonic::ServerInfo) -> Self {
        About { kind: info.kind.clone(), version: info.server_version.clone(), api: info.version.clone() }
    }

    /// "Navidrome 0.53.3", "a Subsonic server (API 1.16.1)".
    pub fn describe(&self) -> String {
        let name = match self.kind.as_str() {
            "" => return if self.api.is_empty() { "a Subsonic server".into() } else { format!("a Subsonic server (API {})", self.api) },
            "navidrome" => "Navidrome".to_owned(),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        };
        // Navidrome's version carries its commit: "0.53.3 (13af8ed4)" reads as "0.53.3".
        let version = self.version.split(" (").next().unwrap_or("").trim();
        if version.is_empty() { name } else { format!("{name} {version}") }
    }
}

/// The original Subsonic file identifies its account by `user`. Plex identifies its server
/// by the machine ID returned at sign-in, even if its address changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Login {
    Plex {
        plex_machine_id: String,
        plex_client_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plex_library: Option<plex::MusicLibrary>,
    },
    Subsonic { user: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub url: String,
    #[serde(flatten)]
    login: Login,
}

impl Account {
    pub fn is_plex(&self) -> bool {
        matches!(self.login, Login::Plex { .. })
    }

    pub fn plex_library(&self) -> Option<&plex::MusicLibrary> {
        match &self.login {
            Login::Plex { plex_library, .. } => plex_library.as_ref(),
            Login::Subsonic { .. } => None,
        }
    }

    pub fn user(&self) -> &str {
        match &self.login {
            Login::Subsonic { user } => user,
            Login::Plex { .. } => "",
        }
    }

    pub fn identity(&self) -> String {
        match &self.login {
            Login::Subsonic { user } => subsonic_identity(&self.url, user),
            Login::Plex { plex_machine_id, .. } => format!("plex:{plex_machine_id}"),
        }
    }

    fn restore(&self, secret: &str) -> Option<Client> {
        match &self.login {
            Login::Subsonic { user } => {
                let (salt, token) = secret.split_once('\n')?;
                if salt.is_empty() || token.is_empty() {
                    return None;
                }
                let credentials = Credentials { url: self.url.clone(), user: user.clone(), salt: salt.into(), token: token.into() };
                subsonic::Client::new(credentials, true).ok().map(Client::Subsonic)
            }
            Login::Plex { plex_machine_id, plex_client_id, plex_library } => {
                let mut client = plex::Client::new(&self.url, secret, plex_machine_id, plex_client_id).ok()?;
                client.select_library(plex_library.as_ref().map(|l| l.key.as_str())).ok()?;
                Some(Client::Plex(client))
            }
        }
    }
}

pub(super) fn subsonic_identity(url: &str, user: &str) -> String {
    format!("{url} ({user})")
}

pub struct Session {
    pub account: Account,
    pub about: About,
    pub client: Client,
}

impl Session {
    pub fn select_plex_library(&mut self, library: plex::MusicLibrary) -> Result<(), Error> {
        let (Client::Plex(client), Login::Plex { plex_library, .. }) = (&mut self.client, &mut self.account.login) else {
            return Err(Error::BadResponse("library selection requires a Plex session".into()));
        };
        client.select_library(Some(&library.key))?;
        *plex_library = Some(library);
        Ok(())
    }
}

/// The secret travels back only to be sealed in the sign-in file.
pub fn sign_in(is_plex: bool, url: &str, user: &str, secret: &str) -> Result<(Session, String), Error> {
    if is_plex {
        let token = secret.trim();
        let (client, info) = plex::sign_in(url, token)?;
        Ok((plex_session(client, info), token.into()))
    } else {
        let (credentials, info) = subsonic::sign_in(url, user, secret)?;
        let account = Account { url: credentials.url.clone(), login: Login::Subsonic { user: credentials.user.clone() } };
        let secret = format!("{}\n{}", credentials.salt, credentials.token);
        let client = Client::Subsonic(subsonic::Client::new(credentials, true)?);
        Ok((Session { account, about: About::of(&info), client }, secret))
    }
}

pub(super) fn plex_session(client: plex::Client, info: plex::ServerInfo) -> Session {
    let account = Account { url: client.base_url().into(), login: Login::Plex { plex_machine_id: info.machine_id, plex_client_id: client.client_id().into(), plex_library: None } };
    let about = About { kind: "Plex Media Server".into(), version: info.version, api: String::new() };
    Session { account, about, client: Client::Plex(client) }
}

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(flatten)]
    account: Account,
    sealed_with: String,
    secret: String,
    #[serde(default)]
    server: About,
}

pub enum Loaded {
    None,
    SignedIn(Box<Session>),
    /// The account to show in Settings when its saved secret cannot be opened.
    Locked(Account),
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

pub fn load(path: &Path) -> Loaded {
    let Ok(text) = std::fs::read_to_string(path) else { return Loaded::None };
    let file: File = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            let mut bad = path.file_name().unwrap_or_default().to_owned();
            bad.push(".bad");
            // Serde errors can quote values from the file. Log only the position.
            if std::fs::copy(path, path.with_file_name(bad)).is_ok() {
                let _ = std::fs::remove_file(path);
                log::error!("{FILE} cannot be read at {}:{}; kept as .bad, signed out", e.line(), e.column());
            } else {
                log::error!("{FILE} cannot be read at {}:{}, nor copied aside; left as it is, signed out", e.line(), e.column());
            }
            return Loaded::None;
        }
    };
    let opened = (file.sealed_with == crate::platform::secret::SCHEME)
        .then(|| unhex(&file.secret))
        .flatten()
        .and_then(|sealed| crate::platform::secret::open(&sealed))
        .and_then(|plain| String::from_utf8(plain).ok());
    match opened.and_then(|secret| file.account.restore(&secret)) {
        Some(client) => Loaded::SignedIn(Box::new(Session { account: file.account, about: file.server, client })),
        None => {
            log::warn!("{FILE}: the saved sign-in cannot be opened here (made for another PC or user, or damaged); sign in again");
            Loaded::Locked(file.account)
        }
    }
}

/// Keep a sign-in. False when it cannot be written (read-only folder, full disk): the app is
/// signed in for this run only.
pub fn save(path: &Path, session: &Session, secret: &str) -> bool {
    let Some(sealed) = crate::platform::secret::seal(secret.as_bytes()) else {
        log::warn!("{FILE}: the token could not be sealed; not saved");
        return false;
    };
    let file = File { account: session.account.clone(), sealed_with: crate::platform::secret::SCHEME.into(), secret: hex(&sealed), server: session.about.clone() };
    let text = serde_json::to_string_pretty(&file).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    let written = crate::platform::secret::write_private(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        log::warn!("{FILE} cannot be saved: {e}");
        return false;
    }
    true
}

/// Keep the new sync scope with the current sign-in. The catalog and cache identify
/// tracks by their server, including when the user changes its selected library.
pub fn save_plex_selection(path: &Path, session: &Session) -> bool {
    let Client::Plex(client) = &session.client else { return false };
    save(path, session, client.token())
}

/// Forget the sign-in (signed out): the file, and the copies made beside it that may hold the
/// sealed token too (`navidrome.json.tmp`, a save cut short; `navidrome.json.bad`, a damaged file
/// kept aside by `load`).
pub fn remove(path: &Path) {
    let name = path.file_name().unwrap_or_default();
    for suffix in ["", ".tmp", ".bad"] {
        let mut n = name.to_owned();
        n.push(suffix);
        if let Err(e) = std::fs::remove_file(path.with_file_name(&n))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("{} cannot be removed: {e}", n.to_string_lossy());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mp-navidrome-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn session() -> Session {
        let c = creds();
        let account = Account { url: c.url.clone(), login: Login::Subsonic { user: c.user.clone() } };
        Session { account, about: About::default(), client: Client::Subsonic(subsonic::Client::new(c, true).unwrap()) }
    }

    fn save_test(path: &Path, about: About) -> bool {
        let session = Session { about, ..session() };
        let c = creds();
        save(path, &session, &format!("{}\n{}", c.salt, c.token))
    }

    fn creds() -> Credentials {
        Credentials { url: "https://music.example.org".into(), user: "ann".into(), salt: "0123456789abcdef".into(), token: "fedcba9876543210fedcba9876543210".into() }
    }

    #[test]
    fn a_sign_in_comes_back_and_its_token_is_not_in_the_file() {
        let d = dir("round");
        let p = path(&d);
        assert!(matches!(load(&p), Loaded::None));
        let about = About { kind: "navidrome".into(), version: "0.53.3 (13af8ed4)".into(), api: "1.16.1".into() };
        assert!(save_test(&p, about.clone()));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("music.example.org") && text.contains("\"ann\""));
        #[cfg(windows)]
        assert!(!text.contains(&creds().token) && !text.contains(&creds().salt), "sealed: {text}");
        assert!(!text.to_lowercase().contains("password"));
        let Loaded::SignedIn(restored) = load(&p) else { panic!("the saved sign-in should open") };
        assert_eq!(restored.about, about);
        assert_eq!(restored.client.as_subsonic().unwrap().credentials(), &creds());
        assert!(!p.with_extension("json.tmp").exists());
        remove(&p);
        assert!(matches!(load(&p), Loaded::None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn plex_restores_the_token_and_its_server_and_client_ids() {
        let d = dir("plex");
        let p = path(&d);
        let token = "fixture-access-token";
        let account = Account {
            url: "https://music.example.org".into(),
            login: Login::Plex { plex_machine_id: "34ab12".into(), plex_client_id: "abc123".into(), plex_library: None },
        };
        let before = Session { client: account.restore(token).unwrap(), about: About { kind: "Plex Media Server".into(), ..Default::default() }, account };
        assert!(save(&p, &before, token));
        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(!saved.contains(token));
        let Loaded::SignedIn(mut after) = load(&p) else { panic!("expected saved Plex connection") };
        assert_eq!(after.account.identity(), "plex:34ab12");
        assert_eq!(after.account, before.account);
        assert!(after.client.same_credentials(&before.client));
        // A Plex sign-in saved before library selection can pin a section in place.
        assert!(!saved.contains("plex_library"));
        let library = plex::MusicLibrary { key: "7".into(), title: "FLAC collection".into() };
        after.select_plex_library(library.clone()).unwrap();
        let invalid = plex::MusicLibrary { key: "invalid/section".into(), title: "Invalid".into() };
        assert!(after.select_plex_library(invalid).is_err());
        assert_eq!(after.account.plex_library(), Some(&library));
        assert!(save_plex_selection(&p, &after));
        let Loaded::SignedIn(restored) = load(&p) else { panic!("expected the same Plex sign-in") };
        assert_eq!(restored.account.plex_library(), Some(&library));
        assert_eq!(restored.account.identity(), before.account.identity());
        assert!(restored.client.same_credentials(&after.client));
        let mut file: serde_json::Value = serde_json::from_str(&saved).unwrap();
        file["sealed_with"] = "unavailable".into();
        std::fs::write(&p, file.to_string()).unwrap();
        assert!(matches!(load(&p), Loaded::Locked(account) if account == before.account));
        remove(&p);
        assert!(matches!(load(&p), Loaded::None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn loads_the_original_untagged_subsonic_file() {
        let d = dir("legacy");
        let p = path(&d);
        let c = creds();
        let secret = crate::platform::secret::seal(format!("{}\n{}", c.salt, c.token).as_bytes()).unwrap();
        let file = serde_json::json!({"url": c.url, "user": c.user, "sealed_with": crate::platform::secret::SCHEME, "secret": hex(&secret)});
        std::fs::write(&p, file.to_string()).unwrap();
        let Loaded::SignedIn(restored) = load(&p) else { panic!("expected legacy Subsonic connection") };
        assert_eq!(restored.account.identity(), "https://music.example.org (ann)");
        assert_eq!(restored.client.as_subsonic().unwrap().credentials(), &c);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Signed out: nothing of the sign-in is left in the folder, not the file, the copy a save
    /// cut short left, nor a damaged one kept aside; the address, user, salt, token and the
    /// sealed hex are in no file there.
    #[test]
    fn signing_out_leaves_nothing_of_the_sign_in() {
        let d = dir("forget");
        let p = path(&d);
        assert!(save_test(&p, About::default()));
        let sealed = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&p).unwrap()).unwrap()["secret"].as_str().unwrap().to_owned();
        std::fs::write(d.join("navidrome.json.tmp"), format!("{{\"secret\":\"{sealed}\"")).unwrap();
        std::fs::copy(&p, d.join("navidrome.json.bad")).unwrap();
        std::fs::write(d.join("other.json"), "{}").unwrap();
        remove(&p);
        let left: Vec<(String, String)> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into_owned(), String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).into_owned()))
            .collect();
        assert_eq!(left.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["other.json"], "only what is not the sign-in's");
        let c = creds();
        for (name, text) in &left {
            for needle in [c.url.as_str(), &c.user, &c.salt, &c.token, &sealed] {
                assert!(!text.contains(needle), "{name} still holds {needle}");
            }
        }
        assert!(matches!(load(&p), Loaded::None), "a restart is signed out");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_damaged_file_is_kept_aside_and_a_foreign_seal_asks_to_sign_in_again() {
        let d = dir("bad");
        let p = path(&d);
        std::fs::write(&p, "{ not json").unwrap();
        assert!(matches!(load(&p), Loaded::None));
        assert!(d.join("navidrome.json.bad").exists() && !p.exists());
        // No copy can be kept aside (here a folder in its place): the file stays where it is.
        std::fs::write(&p, "{ not json").unwrap();
        std::fs::remove_file(d.join("navidrome.json.bad")).unwrap();
        std::fs::create_dir(d.join("navidrome.json.bad")).unwrap();
        assert!(matches!(load(&p), Loaded::None));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json", "not removed without its copy");
        std::fs::remove_dir(d.join("navidrome.json.bad")).unwrap();
        // A seal from elsewhere (another PC's DPAPI blob, or a hand-edited secret).
        assert!(save_test(&p, About::default()));
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["secret"] = "00ff".into();
        std::fs::write(&p, v.to_string()).unwrap();
        #[cfg(windows)]
        assert!(matches!(load(&p), Loaded::Locked(account) if account == session().account));
        v["sealed_with"] = "something-else".into();
        std::fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(load(&p), Loaded::Locked(account) if account == session().account));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn servers_describe_themselves() {
        let a = |k: &str, v: &str, api: &str| About { kind: k.into(), version: v.into(), api: api.into() }.describe();
        assert_eq!(a("navidrome", "0.53.3 (13af8ed4)", "1.16.1"), "Navidrome 0.53.3");
        assert_eq!(a("gonic", "0.16", "1.15.0"), "Gonic 0.16");
        assert_eq!(a("", "", "1.16.1"), "a Subsonic server (API 1.16.1)");
        assert_eq!(a("", "", ""), "a Subsonic server");
        assert_eq!(unhex(&hex(&[0, 1, 254, 255])), Some(vec![0, 1, 254, 255]));
        assert_eq!(unhex("0g"), None);
    }
}
