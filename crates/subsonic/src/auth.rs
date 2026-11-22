//! The server's address as the user typed it, made into a base URL; and what is kept to
//! sign every request: the user, and a salt with its token, never the password.
//!
//! Subsonic's token authentication (API 1.13.0): each request carries `u` (the user),
//! `s` (a salt) and `t = md5(password + salt)` as lowercase hex. The salt is made once, at
//! sign-in, and kept with the token: the server checks the pair against the password it
//! holds, so the password itself is needed only to make the token and is then dropped.
//! (The pair is as good as the password for this server, which is why nothing logs it.)

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::Error;

/// What the app stores to talk to the server (serde: the settings file or the database).
/// `Debug` hides the salt and token, so a credentials value cannot leak through a log line.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// The base URL, normalized ([`normalize_url`]): `https://music.example.org/navidrome`.
    pub url: String,
    pub user: String,
    pub salt: String,
    pub token: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("url", &self.url).field("user", &self.user).field("salt", &"<hidden>").field("token", &"<hidden>").finish()
    }
}

impl Credentials {
    /// From what the user typed: the URL normalized, the user trimmed, a fresh salt and its
    /// token. The password is not kept.
    pub fn from_password(url: &str, user: &str, password: &str) -> Result<Self, Error> {
        let salt = new_salt();
        Ok(Credentials { url: normalize_url(url)?, user: user.trim().to_owned(), token: token(password, &salt), salt })
    }
}

/// The Subsonic token: the MD5 of the password followed by the salt, as lowercase hex.
pub fn token(password: &str, salt: &str) -> String {
    md5::hex(format!("{password}{salt}").as_bytes())
}

/// A fresh salt: 128 bits as 32 hex characters.
///
/// From std's `RandomState`, whose SipHash keys come from the system's random source once per
/// thread, hashing a counter and the clock: unpredictable to anyone without the keys, and
/// unique per call, which is all a salt needs (it travels in every URL beside its token; it
/// only keeps the token from being the password's plain MD5). No crate added for it.
pub fn new_salt() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut out = String::with_capacity(32);
    for half in 0..2u8 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(nanos);
        h.write_u8(half);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// The base URL from what the user typed: trimmed; `https://` added when there is no scheme;
/// the scheme and host lowercased; a path kept ("/navidrome") without its trailing slash or a
/// trailing "/rest"; query and fragment dropped. Plain http is accepted (a server at home);
/// [`is_plain_http`] tells the UI to say so.
///
/// Refused ([`Error::BadUrl`]): nothing typed, another scheme, no host, a bad port, spaces,
/// and a user or password in the address ("https://me:secret@host"): sign-in takes those
/// separately, and an address is shown and logged where a password must never be.
pub fn normalize_url(input: &str) -> Result<String, Error> {
    http_client::normalize_base_url(input, &["/rest"]).ok_or(Error::BadUrl)
}

/// Whether the address (as typed or stored) is plain http, so the token would cross the
/// network readable: the UI warns, and lets the user go on (a server on the home network).
/// False for anything that is not a valid address.
pub fn is_plain_http(url: &str) -> bool {
    normalize_url(url).is_ok_and(|u| u.starts_with("http://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_md5_of_password_and_salt() {
        // The example in the Subsonic API documentation.
        assert_eq!(token("sesame", "c19b2d"), "26719a1196d2a940705a59634eb18eab");
    }

    #[test]
    fn salts_are_fresh_hex() {
        let a = new_salt();
        let b = new_salt();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn credentials_hide_their_secrets() {
        let c = Credentials::from_password(" music.example.org/nd/ ", " bob ", "pw").unwrap();
        assert_eq!(c.url, "https://music.example.org/nd");
        assert_eq!(c.user, "bob");
        assert_eq!(c.token, token("pw", &c.salt));
        let shown = format!("{c:?}");
        assert!(!shown.contains(&c.token) && !shown.contains(&c.salt), "{shown}");
        // And they round-trip through serde, as stored.
        let back: Credentials = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn urls_normalize() {
        let n = |s: &str| normalize_url(s);
        assert_eq!(n("music.example.com"), Ok("https://music.example.com".into()));
        assert_eq!(n(" https://Music.Example.com/ "), Ok("https://music.example.com".into()));
        assert_eq!(n("https://music.example.com/nd/rest/"), Ok("https://music.example.com/nd".into()));
        assert_eq!(n("HTTP://192.168.1.10:4533"), Ok("http://192.168.1.10:4533".into()));
        assert_eq!(n("http://host:4533/navidrome/?x=1#y"), Ok("http://host:4533/navidrome".into()));
        assert_eq!(n("http://[::1]:4533"), Ok("http://[::1]:4533".into()));
        assert_eq!(n("nas.local:4533"), Ok("https://nas.local:4533".into()));
        for bad in ["", "   ", "ftp://x", "https://", "https://:80", "https://h:99999", "https://h:0", "https://me:pw@h", "https://a b", "https://h/x y"] {
            assert_eq!(n(bad), Err(Error::BadUrl), "{bad:?}");
        }
    }

    #[test]
    fn plain_http_is_told() {
        assert!(is_plain_http("http://192.168.1.10:4533"));
        assert!(is_plain_http("HTTP://nas"));
        assert!(!is_plain_http("https://nas"));
        assert!(!is_plain_http("nas"), "no scheme means https");
        assert!(!is_plain_http("ftp://nas"));
    }
}
