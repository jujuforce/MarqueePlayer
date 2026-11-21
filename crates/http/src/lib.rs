//! A small blocking HTTP client with timeouts, shared by the Last.fm scrobbler, the
//! ListenBrainz submitter, the lyrics lookup and the Subsonic client; each calls it only from its
//! own worker thread, never the UI thread.
//!
//! One interface, one implementation per system: WinHTTP on Windows (`winhttp.rs`:
//! Windows' own HTTP stack and TLS, nothing added to the exe); ureq with rustls on Linux
//! (`ureq.rs`: certificates checked against the system's root store, proxies from the usual
//! environment variables). Each implementation does one thing, [`download`]: send a request
//! and hand the body to a [`Sink`] as it arrives. The rest is built on it here:
//!
//! - [`request`] / [`get`] / [`post`]: the whole body as text (JSON APIs).
//! - [`request_bytes`]: the whole body as bytes, with its `Content-Type` (cover art).
//! - [`download`]: the body chunk by chunk, never held whole (a song file of several hundred
//!   MB), with what the head said first (status, length, range, type, validators), and a way
//!   to stop.
//!
//! Redirects are followed, up to 10. A request that carries an `Authorization` header, or
//! one made through [`request_same_server`] / [`request_bytes_same_server`] /
//! [`download_same_server`] (for credentials carried in the URL, as Subsonic does), follows
//! them here instead, alike on both systems: only to the same scheme, host and port, with the
//! headers kept; a redirect anywhere else (including a downgrade from https to http) is an
//! error, so a sign-in never goes to another server and never goes missing on the way (the
//! server would say "not signed in"). Otherwise each system follows them its own way (WinHTTP
//! never from https to http; ureq to anywhere). Nothing here logs, and no error message
//! carries a URL: the Subsonic client's URLs hold its token.

pub mod audio;

use std::ops::ControlFlow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// No connection, DNS failure, timeout, a connection closed mid-body: worth trying
    /// again later.
    Network(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Network(m) => write!(f, "network: {m}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// What came back, as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    /// `Retry-After`, in seconds, when the server sent one (a 429 or 503).
    pub retry_after: Option<u64>,
    /// The rate-limit headers, when the server sent them (ListenBrainz).
    pub rate_limit: RateLimit,
}

/// A service's rate-limit headers, as ListenBrainz sends them on every answer:
/// `X-RateLimit-Remaining` (requests left in this window) and `X-RateLimit-Reset-In` (seconds
/// until the window ends; the clock-independent one of the two it sends).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: Option<u64>,
    pub reset_in: Option<u64>,
}

/// What came back, as bytes (an image), with its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// `Content-Type` as sent ("image/jpeg"), when there was one.
    pub content_type: Option<String>,
    pub retry_after: Option<u64>,
    pub rate_limit: RateLimit,
}

/// The base address of a user-configured server. Keep reverse proxy paths, drop the query
/// and fragment, and strip a known endpoint suffix when an address was pasted from a browser.
/// An address with userinfo is never retained, as this value may be displayed in Settings.
pub fn normalize_base_url(input: &str, endpoint_suffixes: &[&str]) -> Option<String> {
    let s = input.trim();
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return None;
    }
    let (scheme, rest) = match s.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_owned(), s),
    };
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (h, Some(p)),
        _ => (authority, None),
    };
    let host_ok = if let Some(v6) = host.strip_prefix('[') {
        v6.strip_suffix(']').is_some_and(|a| !a.is_empty() && a.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.'))
    } else {
        !host.is_empty() && host.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_'))
    };
    if !host_ok {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|&n| n > 0)?),
        None => None,
    };
    let mut path = path.trim_end_matches('/');
    if let Some(p) = endpoint_suffixes.iter().find_map(|suffix| path.strip_suffix(suffix)) {
        path = p.trim_end_matches('/');
    }
    let mut url = format!("{scheme}://{}", host.to_lowercase());
    if let Some(p) = port {
        url.push_str(&format!(":{p}"));
    }
    url.push_str(path);
    Some(url)
}

/// One request.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    /// Extra headers, each "Name: value" (a `Range: bytes=N-` for a download that resumes).
    pub headers: &'a [&'a str],
    pub body: &'a [u8],
    pub user_agent: &'a str,
    /// For each of resolve, connect, send and receive. Receiving is timed per read: a body
    /// that keeps coming may take as long as it takes ([`download`]; the buffered calls end at
    /// [`MAX_TIME`]), one that stalls for this long ends in an error.
    pub timeout_ms: u32,
}

/// What the response's head said, before any of its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    /// `Content-Length`: the bytes of this body (of the range, for a 206). None for a
    /// chunked body (a live transcode) or one that ends when the connection closes.
    pub content_length: Option<u64>,
    /// `Content-Range` as sent ("bytes 1000-9999/10000"); [`Head::range`] reads it.
    pub content_range: Option<String>,
    pub content_type: Option<String>,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
    pub rate_limit: RateLimit,
    /// `Location` as sent, on a redirect the system's client did not follow.
    pub location: Option<String>,
    /// `ETag` as sent (quotes and any `W/` kept): with `last_modified`, what tells this version
    /// of the resource from another; [`Head::validator`] picks the one an `If-Range` can carry.
    pub etag: Option<String>,
    /// `Last-Modified` as sent (an HTTP date, not read).
    pub last_modified: Option<String>,
}

/// A `Content-Range` of bytes: the first and last byte this body holds, and the length of
/// the whole when the server knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub first: u64,
    pub last: u64,
    pub total: Option<u64>,
}

impl Head {
    /// The `Content-Range`, read: None when there is none, or it is the unsatisfied form
    /// ("bytes */1000") or not bytes.
    pub fn range(&self) -> Option<ContentRange> {
        let s = self.content_range.as_deref()?.trim();
        let s = s.strip_prefix("bytes")?.trim_start();
        let (span, total) = s.split_once('/')?;
        let (first, last) = span.trim().split_once('-')?;
        let (first, last) = (first.trim().parse().ok()?, last.trim().parse().ok()?);
        let total = match total.trim() {
            "*" => None,
            t => Some(t.parse().ok()?),
        };
        (first <= last).then_some(ContentRange { first, last, total })
    }

    /// What an `If-Range` sent with a later `Range` request may carry, so that the range comes
    /// only from this same version of the resource (else the server sends all of it, with 200):
    /// the `ETag` when it is a strong one, else `Last-Modified`; None when there is neither (a
    /// weak `ETag` cannot be used there).
    pub fn validator(&self) -> Option<&str> {
        let strong = self.etag.as_deref().map(str::trim).filter(|e| e.starts_with('"') && e.len() >= 2);
        strong.or_else(|| self.last_modified.as_deref().map(str::trim).filter(|d| !d.is_empty()))
    }
}

/// Where a [`download`] goes. Either call can stop it by returning `Break`: the connection is
/// dropped there and then, without reading the rest.
pub trait Sink {
    /// The head, before any of the body (also for an error status: the caller decides).
    fn head(&mut self, head: &Head) -> ControlFlow<()>;
    /// The next bytes of the body, as they came in (at most 64 KiB at a time).
    fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()>;
}

/// How a [`download`] ended, when it did not fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub head: Head,
    /// Body bytes given to the sink.
    pub received: u64,
    /// The sink stopped it; otherwise the body was read to its end.
    pub stopped: bool,
}

/// The most of a body [`request`] and [`request_bytes`] hold (JSON answers are kilobytes, a
/// cover a few MB): beyond it they fail instead of filling memory.
pub const MAX_BODY: u64 = 64 << 20;

/// The longest [`request`] and [`request_bytes`] take, start to end: each read is timed on its
/// own, so a server sending a byte now and then would otherwise hold the caller's thread (the
/// scrobbler's, the lyrics lookup's) for as long as it likes. Streamed downloads have no such
/// bound (a song may take minutes on a slow link).
pub const MAX_TIME: std::time::Duration = std::time::Duration::from_secs(300);

/// Bytes read at a time.
const CHUNK: usize = 64 * 1024;

/// Percent-encode one value (UTF-8, every byte but the unreserved ones escaped), for a
/// query string or a form body.
pub fn encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// `application/x-www-form-urlencoded` (UTF-8, every reserved byte escaped); also a query
/// string.
pub fn form_encode(params: &[(String, String)]) -> String {
    params.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

/// POST a form `body` to `url` as `user_agent`; the status code and the response body.
pub fn post(url: &str, body: &str, user_agent: &str, timeout_ms: u32) -> Result<(u16, String), HttpError> {
    let r = request(&Request {
        method: "POST",
        url,
        headers: &["Content-Type: application/x-www-form-urlencoded"],
        body: body.as_bytes(),
        user_agent,
        timeout_ms,
    })?;
    Ok((r.status, r.body))
}

/// GET `url`.
pub fn get(url: &str, user_agent: &str, timeout_ms: u32) -> Result<Response, HttpError> {
    request(&Request { method: "GET", url, headers: &[], body: &[], user_agent, timeout_ms })
}

/// Send one request and read the whole response as text (any status: the caller decides).
/// Bytes that are not UTF-8 become U+FFFD.
pub fn request(req: &Request) -> Result<Response, HttpError> {
    let r = request_bytes(req)?;
    Ok(Response { status: r.status, body: String::from_utf8_lossy(&r.body).into_owned(), retry_after: r.retry_after, rate_limit: r.rate_limit })
}

/// Like [`request`], but follows redirects only to the same scheme, host and port (for
/// requests whose sign-in travels in the URL rather than an `Authorization` header).
pub fn request_same_server(req: &Request) -> Result<Response, HttpError> {
    let r = request_bytes_same_server(req)?;
    Ok(Response { status: r.status, body: String::from_utf8_lossy(&r.body).into_owned(), retry_after: r.retry_after, rate_limit: r.rate_limit })
}

/// Send one request and read the whole response as bytes (any status), up to [`MAX_BODY`], in
/// [`MAX_TIME`] at most.
pub fn request_bytes(req: &Request) -> Result<BinaryResponse, HttpError> {
    request_within(req, MAX_TIME, false)
}

/// Like [`request_bytes`], but follows redirects only to the same scheme, host and port.
pub fn request_bytes_same_server(req: &Request) -> Result<BinaryResponse, HttpError> {
    request_within(req, MAX_TIME, true)
}

fn request_within(req: &Request, max_time: std::time::Duration, same_server_only: bool) -> Result<BinaryResponse, HttpError> {
    struct Collect {
        body: Vec<u8>,
        too_big: bool,
        deadline: std::time::Instant,
        too_long: bool,
    }
    impl Collect {
        fn late(&mut self) -> bool {
            self.too_long = std::time::Instant::now() >= self.deadline;
            self.too_long
        }
    }
    impl Sink for Collect {
        fn head(&mut self, head: &Head) -> ControlFlow<()> {
            if self.late() {
                return ControlFlow::Break(());
            }
            if head.content_length.is_some_and(|n| n > MAX_BODY) {
                self.too_big = true;
                return ControlFlow::Break(());
            }
            self.body.reserve(head.content_length.unwrap_or(0) as usize);
            ControlFlow::Continue(())
        }
        fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
            if self.late() {
                return ControlFlow::Break(());
            }
            if (self.body.len() + bytes.len()) as u64 > MAX_BODY {
                self.too_big = true;
                return ControlFlow::Break(());
            }
            self.body.extend_from_slice(bytes);
            ControlFlow::Continue(())
        }
    }
    let mut c = Collect { body: Vec::new(), too_big: false, deadline: std::time::Instant::now() + max_time, too_long: false };
    let d = download_with_policy(req, &mut c, same_server_only)?;
    if c.too_big {
        return Err(HttpError::Network(format!("the response is larger than {} MB", MAX_BODY >> 20)));
    }
    if c.too_long {
        return Err(HttpError::Network(format!("the response took longer than {} s", max_time.as_secs())));
    }
    Ok(BinaryResponse { status: d.head.status, body: c.body, content_type: d.head.content_type, retry_after: d.head.retry_after, rate_limit: d.head.rate_limit })
}

/// Send one request and hand its body to `sink` as it arrives, never holding more than a
/// chunk of it: first the head, then the body in chunks, until the end, an error or the sink
/// stops it.
///
/// A stop returns at once (the connection is dropped, not drained). A connection that goes
/// quiet for the receive timeout ends in an error; so does one that closes before the
/// `Content-Length` it announced, after the sink has had every byte that did come. A cancel
/// flag of the caller's is best checked in [`Sink::chunk`]: while the server sends nothing
/// the call waits in the system's receive, which only the timeout ends.
pub fn download(req: &Request, sink: &mut dyn Sink) -> Result<Download, HttpError> {
    download_with_policy(req, sink, false)
}

/// Like [`download`], but follows redirects only to the same scheme, host and port (for
/// requests whose sign-in travels in the URL rather than an `Authorization` header).
pub fn download_same_server(req: &Request, sink: &mut dyn Sink) -> Result<Download, HttpError> {
    download_with_policy(req, sink, true)
}

fn download_with_policy(req: &Request, sink: &mut dyn Sink, same_server_only: bool) -> Result<Download, HttpError> {
    /// Counts what passes, so a short body can be told from a whole one.
    struct Counting<'a> {
        inner: &'a mut dyn Sink,
        received: u64,
    }
    impl Sink for Counting<'_> {
        fn head(&mut self, head: &Head) -> ControlFlow<()> {
            self.inner.head(head)
        }
        fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
            self.received += bytes.len() as u64;
            self.inner.chunk(bytes)
        }
    }
    let mut counting = Counting { inner: sink, received: 0 };
    let signed_in = same_server_only
        || req.headers.iter().any(|h| h.split_once(':').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("authorization")));
    let (head, stopped) = if signed_in { follow_same_server(req, &mut counting)? } else { system(req, &mut counting, true)? };
    let received = counting.received;
    let has_body = !req.method.eq_ignore_ascii_case("HEAD") && !matches!(head.status, 100..=199 | 204 | 304);
    if !stopped
        && has_body
        && let Some(expected) = head.content_length
        && received < expected
    {
        return Err(HttpError::Network(format!("the connection closed after {received} of {expected} bytes")));
    }
    Ok(Download { head, received, stopped })
}

/// One request on this system's client; `follow`: let it follow redirects itself.
fn system(req: &Request, sink: &mut dyn Sink, follow: bool) -> Result<(Head, bool), HttpError> {
    #[cfg(windows)]
    return winhttp::download(req, sink, follow);
    #[cfg(not(windows))]
    return ureq::download(req, sink, follow);
}

/// A signed-in request (carrying an `Authorization` header or made via `*_same_server`):
/// redirects followed here, only to the same scheme, host and port, headers sent again; one
/// elsewhere (including https to http) is an error. A 303, or a 301/302 after a POST, goes
/// on as a GET without the body, as the systems' clients do.
fn follow_same_server(req: &Request, sink: &mut dyn Sink) -> Result<(Head, bool), HttpError> {
    /// Keeps a redirect's head and body from the caller's sink.
    struct Hold<'a> {
        inner: &'a mut dyn Sink,
        redirect: Option<(u16, String)>,
    }
    impl Sink for Hold<'_> {
        fn head(&mut self, head: &Head) -> ControlFlow<()> {
            if let (301 | 302 | 303 | 307 | 308, Some(l)) = (head.status, &head.location) {
                self.redirect = Some((head.status, l.clone()));
                return ControlFlow::Break(());
            }
            self.inner.head(head)
        }
        fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
            self.inner.chunk(bytes)
        }
    }
    let (mut url, mut method, mut body) = (req.url.to_owned(), req.method, req.body);
    for _ in 0..=10 {
        let mut hold = Hold { inner: &mut *sink, redirect: None };
        let (head, stopped) = system(&Request { method, url: &url, body, ..req.clone() }, &mut hold, false)?;
        let Some((status, location)) = hold.redirect else { return Ok((head, stopped)) };
        let next = same_server_redirect(&url, &location)?;
        if status == 303 || (matches!(status, 301 | 302) && method.eq_ignore_ascii_case("POST")) {
            method = "GET";
            body = &[];
        }
        url = next;
    }
    Err(HttpError::Network("too many redirects".into()))
}

/// Resolve `location` against `from` and check that it stays on the same scheme, host and
/// port; a scheme change (such as https to http), host change or port change is refused.
fn same_server_redirect(from: &str, location: &str) -> Result<String, HttpError> {
    let next = resolve(from, location);
    let server = |url: &str| split_url(url).map(|(secure, host, port, _)| (secure, host.to_ascii_lowercase(), port));
    if server(&next)? != server(from)? {
        return Err(HttpError::Network("the server redirects to another address (another scheme, host or port), where the sign-in is not sent: use that address instead".into()));
    }
    Ok(next)
}

/// A `Location` read against the URL it came from: absolute, `//host/...`, `/path` or relative.
fn resolve(base: &str, location: &str) -> String {
    let location = location.trim();
    if let Some(rest) = location.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).map(|_| &location[8..]) {
        return format!("https://{rest}");
    }
    if let Some(rest) = location.get(..7).filter(|s| s.eq_ignore_ascii_case("http://")).map(|_| &location[7..]) {
        return format!("http://{rest}");
    }
    // Another URI scheme ("ftp://...", "file:...") stays absolute so `split_url` rejects it.
    if let Some(colon) = location.find(':') {
        let prefix = &location.as_bytes()[..colon];
        if let Some((&first, rest)) = prefix.split_first()
            && first.is_ascii_alphabetic()
            && rest.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        {
            return location.to_owned();
        }
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    if let Some(rest) = location.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme_end]);
    }
    let path_start = base[scheme_end..].find(['/', '?']).map_or(base.len(), |i| scheme_end + i);
    if location.starts_with('/') {
        return format!("{}{location}", &base[..path_start]);
    }
    let path = &base[path_start..];
    let path = &path[..path.find('?').unwrap_or(path.len())];
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    format!("{}{}{location}", &base[..path_start], if dir.is_empty() { "/" } else { dir })
}

/// Split an http(s) URL into (secure, host, port, path with query). The error never repeats
/// the URL (it may carry a token).
fn split_url(url: &str) -> Result<(bool, &str, u16, String), HttpError> {
    let (secure, rest) = if url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
        (true, &url[8..])
    } else if url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
        (false, &url[7..])
    } else {
        return Err(HttpError::Network("not an http(s) URL".into()));
    };
    let (hostport, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_owned()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_owned()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        // An IPv6 literal ("[::1]") has colons of its own: a port only follows its ']'.
        Some((h, p)) if !p.contains(']') => (h, p.parse::<u16>().map_err(|_| HttpError::Network("bad port".into()))?),
        _ => (hostport, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(HttpError::Network("no host in the URL".into()));
    }
    Ok((secure, host, port, path))
}

/// `Retry-After` in seconds (the HTTP-date form is not used by the services here).
fn parse_retry_after(v: &str) -> Option<u64> {
    v.trim().parse::<u64>().ok()
}

/// The rate-limit headers a response carries (`header` gives one by name, None when absent).
fn rate_limit(header: impl Fn(&str) -> Option<String>) -> RateLimit {
    let n = |name: &str| header(name).and_then(|v| v.trim().parse::<u64>().ok());
    RateLimit { remaining: n("X-RateLimit-Remaining"), reset_in: n("X-RateLimit-Reset-In") }
}

#[cfg(windows)]
mod winhttp;

#[cfg(not(windows))]
mod ureq;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_escapes_utf8_and_reserved() {
        let p = [("artist[0]".to_owned(), "Beyoncé & Jay-Z".to_owned()), ("track".to_owned(), "a+b=c/d 東".to_owned())];
        assert_eq!(form_encode(&p), "artist%5B0%5D=Beyonc%C3%A9%20%26%20Jay-Z&track=a%2Bb%3Dc%2Fd%20%E6%9D%B1");
    }

    /// A server that sends its body a byte at a time, each well within the receive timeout: the
    /// buffered call still ends, at its deadline.
    #[test]
    fn a_body_that_dribbles_ends_at_the_deadline() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let Ok((mut c, _)) = listener.accept() else { return };
            let _ = c.read(&mut [0u8; 4096]);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n");
            for _ in 0..1000 {
                if c.write_all(b"x").and_then(|()| c.flush()).is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        let req = Request { method: "GET", url: &url, headers: &[], body: &[], user_agent: "MarqueePlayer-test/1", timeout_ms: 5000 };
        let t = std::time::Instant::now();
        let e = request_within(&req, std::time::Duration::from_millis(800), false).unwrap_err();
        assert!(e.to_string().contains("took longer"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(4), "{:?}", t.elapsed());
    }

    #[test]
    fn redirect_locations_resolve() {
        let b = "http://nas:8100/apis/lb/1/submit?x=1";
        assert_eq!(resolve(b, "https://other/y"), "https://other/y");
        assert_eq!(resolve(b, "//other:1/y"), "http://other:1/y");
        assert_eq!(resolve(b, "/1/submit/"), "http://nas:8100/1/submit/");
        assert_eq!(resolve(b, "submit/"), "http://nas:8100/apis/lb/1/submit/");
        assert_eq!(resolve("http://h?q", "a"), "http://h/a");
        assert_eq!(resolve("http://h", "/a"), "http://h/a");
        assert_eq!(resolve(b, "HTTPS://other/y"), "https://other/y");
        assert_eq!(resolve(b, "ftp://other/y"), "ftp://other/y");
    }

    #[test]
    fn same_server_redirect_policy_checks_scheme_host_and_port() {
        let from = "https://music.example.org/rest/ping.view?u=alice&t=secrettoken&s=secretsalt";
        // Same server: relative, root-relative, explicit default port, case-insensitive host/scheme.
        assert_eq!(same_server_redirect(from, "/nd/rest/ping.view?u=alice").unwrap(), "https://music.example.org/nd/rest/ping.view?u=alice");
        assert_eq!(same_server_redirect(from, "ping2.view").unwrap(), "https://music.example.org/rest/ping2.view");
        assert_eq!(same_server_redirect(from, "https://MUSIC.EXAMPLE.ORG:443/rest/ping.view").unwrap(), "https://MUSIC.EXAMPLE.ORG:443/rest/ping.view");
        assert_eq!(same_server_redirect(from, "HTTPS://music.example.org/rest/ping.view").unwrap(), "https://music.example.org/rest/ping.view");
        assert_eq!(same_server_redirect("http://nas/rest/ping.view", "http://nas:80/v2/rest/ping.view").unwrap(), "http://nas:80/v2/rest/ping.view");

        // Scheme downgrade (https -> http), scheme change (http -> https), host change, port change,
        // non-http(s) scheme: all refused without leaking the query string.
        for (src, loc) in [
            (from, "http://music.example.org/rest/ping.view?u=alice&t=secrettoken"),
            (from, "http://music.example.org:443/rest/ping.view?t=secrettoken"),
            (from, "HTTP://music.example.org/rest/ping.view?t=secrettoken"),
            ("https://music.example.org:8443/rest/ping.view?t=secrettoken", "http://music.example.org:8443/rest/ping.view?t=secrettoken"),
            ("http://music.example.org/rest/ping.view?t=secrettoken", "https://music.example.org/rest/ping.view?t=secrettoken"),
            (from, "https://evil.example.org/rest/ping.view?t=secrettoken"),
            (from, "//evil.example.org/rest/ping.view?t=secrettoken"),
            (from, "https://music.example.org:4533/rest/ping.view?t=secrettoken"),
            (from, "ftp://music.example.org/rest/ping.view?t=secrettoken"),
        ] {
            let e = same_server_redirect(src, loc).unwrap_err().to_string();
            assert!(!e.contains("secrettoken") && !e.contains("secretsalt"), "secret leaked for {loc}: {e}");
        }
    }

    #[test]
    fn urls_split() {
        assert_eq!(split_url("https://lrclib.net/api/get?x=1").unwrap(), (true, "lrclib.net", 443, "/api/get?x=1".to_owned()));
        assert_eq!(split_url("http://127.0.0.1:8080").unwrap(), (false, "127.0.0.1", 8080, "/".to_owned()));
        assert_eq!(split_url("http://h:1?q").unwrap(), (false, "h", 1, "/?q".to_owned()));
        assert_eq!(split_url("http://[::1]:4533/x").unwrap(), (false, "[::1]", 4533, "/x".to_owned()));
        assert!(split_url("ftp://x").is_err());
        assert!(split_url("http://:80/").is_err());
    }

    #[test]
    fn url_errors_never_repeat_the_url() {
        let e = split_url("ftp://h/rest/ping?t=secrettoken").unwrap_err().to_string();
        assert!(!e.contains("secrettoken"), "{e}");
        let e = split_url("http://h:xx/rest/ping?t=secrettoken").unwrap_err().to_string();
        assert!(!e.contains("secrettoken"), "{e}");
    }

    #[test]
    fn rate_limit_headers_read() {
        let h = |name: &str| match name {
            "X-RateLimit-Remaining" => Some(" 0 ".to_owned()),
            "X-RateLimit-Reset-In" => Some("7".to_owned()),
            _ => None,
        };
        assert_eq!(rate_limit(h), RateLimit { remaining: Some(0), reset_in: Some(7) });
        assert_eq!(rate_limit(|_| Some("soon".to_owned())), RateLimit::default());
        assert_eq!(rate_limit(|_| None), RateLimit::default());
    }

    #[test]
    fn content_ranges_read() {
        let h = |r: &str| Head { content_range: Some(r.into()), ..Head::default() }.range();
        assert_eq!(h("bytes 100-999/1000"), Some(ContentRange { first: 100, last: 999, total: Some(1000) }));
        assert_eq!(h("bytes 0-0/*"), Some(ContentRange { first: 0, last: 0, total: None }));
        assert_eq!(h("bytes */1000"), None);
        assert_eq!(h("items 1-2/3"), None);
        assert_eq!(h("bytes 9-1/10"), None);
        assert_eq!(Head::default().range(), None);
    }

    #[test]
    fn validators_prefer_a_strong_etag() {
        let lm = "Wed, 21 Oct 2026 07:28:00 GMT";
        let h = |etag: Option<&str>, last_modified: Option<&str>| Head { etag: etag.map(Into::into), last_modified: last_modified.map(Into::into), ..Head::default() };
        assert_eq!(h(Some("\"abc\""), Some(lm)).validator(), Some("\"abc\""));
        assert_eq!(h(Some("W/\"abc\""), Some(lm)).validator(), Some(lm));
        assert_eq!(h(Some("W/\"abc\""), None).validator(), None);
        assert_eq!(h(None, Some(lm)).validator(), Some(lm));
        assert_eq!(h(None, Some("  ")).validator(), None);
        assert_eq!(Head::default().validator(), None);
    }
}
