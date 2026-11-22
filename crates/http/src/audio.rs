//! Resumable audio downloads shared by the music server clients.

use std::ops::ControlFlow;
use crate::{Head, HttpError, Request};

/// Error bodies may carry a protocol-specific explanation.
const ERROR_BODY: usize = 64 * 1024;

#[derive(Debug)]
pub enum StreamError {
    Network(HttpError),
    BadRange,
    Response { status: u16, content_type: Option<String>, body: Vec<u8> },
}

/// What a stream's head said, before its body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamHead {
    /// The offset in the file of the first byte the body holds: the asked start when the
    /// range was honoured, else 0 (the whole file comes, from its start).
    pub start: u64,
    /// The asked start was honoured (a 206 from it). Always true for a start of 0.
    pub range_honoured: bool,
    /// Bytes of this body, when the server said (None for a live transcode).
    pub length: Option<u64>,
    /// Bytes of the whole file, when known: from `Content-Range`, or the length of a body
    /// that is the whole file.
    pub total: Option<u64>,
    pub content_type: Option<String>,
    /// What tells this version of the file from another (`crate::Head::validator`: its
    /// strong `ETag`, else its `Last-Modified`), when the server sent one: given back to
    /// [`download`] when it goes on from a range, so a range of a file changed since comes
    /// as the whole new file instead (a 200, `range_honoured` false).
    pub validator: Option<String>,
}

/// Where a stream goes; either call can stop it by returning `Break` (it then returns at
/// once, without reading the rest).
pub trait StreamSink {
    fn start(&mut self, head: &StreamHead) -> ControlFlow<()>;
    fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()>;
}

/// How a stream ended, when it did not fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Streamed {
    pub head: StreamHead,
    /// Body bytes given to the sink.
    pub received: u64,
    /// The sink stopped it; otherwise the body was read to its end.
    pub stopped: bool,
}

/// Stream the original response bytes, following redirects only on the same server.
/// Keep textual errors out of the sink so the calling protocol can classify them.
pub fn download(req: &Request<'_>, range_start: u64, if_range: Option<&str>, sink: &mut dyn StreamSink) -> Result<Streamed, StreamError> {
    let range = format!("Range: bytes={range_start}-");
    // A validator with a line break in it would be another header: not sent.
    let if_range = if_range.map(str::trim).filter(|v| !v.is_empty() && !v.contains(['\r', '\n'])).map(|v| format!("If-Range: {v}"));
    let mut headers = req.headers.to_vec();
    if range_start > 0 {
        headers.push(&range);
        headers.extend(if_range.as_deref());
    }
    let req = Request { headers: &headers, ..*req };

    /// Sorts the head into audio (to the sink) or an error answer (kept, to read).
    struct Adapter<'a> {
        sink: &'a mut dyn StreamSink,
        range_start: u64,
        head: Option<StreamHead>,
        error: Option<(u16, Option<String>, Vec<u8>)>,
        bad: Option<StreamError>,
        empty_end: bool,
    }
    impl crate::Sink for Adapter<'_> {
        fn head(&mut self, h: &Head) -> ControlFlow<()> {
            let ct = h.content_type.as_deref().unwrap_or("").to_ascii_lowercase();
            let answer_not_audio = ct.contains("json") || ct.contains("xml") || ct.starts_with("text/");
            if h.status == 416 && h.content_range.as_deref().and_then(total_of_unsatisfied) == Some(self.range_start) {
                // Asked from the end of the file: nothing more to send.
                let head = StreamHead {
                    start: self.range_start,
                    range_honoured: true,
                    length: Some(0),
                    total: Some(self.range_start),
                    content_type: h.content_type.clone(),
                    validator: h.validator().map(str::to_owned),
                };
                self.empty_end = true;
                // Whatever the sink says, there is no body to read.
                let _ = self.sink.start(&head);
                self.head = Some(head);
                return ControlFlow::Break(());
            }
            if !(200..300).contains(&h.status) || answer_not_audio {
                self.error = Some((h.status, h.content_type.clone(), Vec::new()));
                return ControlFlow::Continue(());
            }
            let head = match stream_head(h, self.range_start) {
                Ok(head) => head,
                Err(e) => {
                    self.bad = Some(e);
                    return ControlFlow::Break(());
                }
            };
            let flow = self.sink.start(&head);
            self.head = Some(head);
            flow
        }
        fn chunk(&mut self, bytes: &[u8]) -> ControlFlow<()> {
            if let Some((_, _, body)) = &mut self.error {
                body.extend_from_slice(&bytes[..bytes.len().min(ERROR_BODY - body.len())]);
                return if body.len() >= ERROR_BODY { ControlFlow::Break(()) } else { ControlFlow::Continue(()) };
            }
            self.sink.chunk(bytes)
        }
    }

    let mut a = Adapter { sink, range_start, head: None, error: None, bad: None, empty_end: false };
    let r = crate::download_same_server(&req, &mut a);
    if let Some(e) = a.bad.take() {
        return Err(e);
    }
    if let Some((status, ct, body)) = a.error.take() {
        return Err(StreamError::Response { status, content_type: ct, body });
    }
    let d = r.map_err(StreamError::Network)?;
    let head = a.head.unwrap_or_default();
    let stopped = d.stopped && !a.empty_end;
    Ok(Streamed { head, received: d.received, stopped })
}

/// Build a [`StreamHead`] from a 2xx audio response's HTTP head; fails when a 206's `Content-Range` span overflows `u64`.
fn stream_head(h: &Head, range_start: u64) -> Result<StreamHead, StreamError> {
    let range = h.range().filter(|_| h.status == 206);
    match range {
        Some(r) => {
            let range_len = r.last.checked_sub(r.first).and_then(|d| d.checked_add(1)).ok_or(StreamError::BadRange)?;
            Ok(StreamHead {
                start: r.first,
                range_honoured: r.first == range_start,
                length: h.content_length.or(Some(range_len)),
                total: r.total,
                content_type: h.content_type.clone(),
                validator: h.validator().map(str::to_owned),
            })
        }
        None => Ok(StreamHead {
            start: 0,
            range_honoured: range_start == 0,
            length: h.content_length,
            total: h.content_length,
            content_type: h.content_type.clone(),
            validator: h.validator().map(str::to_owned),
        }),
    }
}

/// The total of an unsatisfied `Content-Range` ("bytes */1000").
fn total_of_unsatisfied(content_range: &str) -> Option<u64> {
    content_range.trim().strip_prefix("bytes")?.trim().strip_prefix("*/")?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsatisfied_ranges_read() {
        assert_eq!(total_of_unsatisfied("bytes */1000"), Some(1000));
        assert_eq!(total_of_unsatisfied("bytes 0-1/2"), None);
    }

    #[test]
    fn overflowing_content_range_is_rejected() {
        let hostile = Head {
            status: 206,
            content_length: None,
            content_range: Some("bytes 0-18446744073709551615/*".into()),
            content_type: Some("audio/flac".into()),
            ..Head::default()
        };
        assert!(matches!(stream_head(&hostile, 0), Err(StreamError::BadRange)));
        let hostile_with_len = Head { content_length: Some(5), ..hostile };
        assert!(matches!(stream_head(&hostile_with_len, 0), Err(StreamError::BadRange)));

        // Boundary range that does not overflow: 1..=u64::MAX has length u64::MAX.
        let max_valid = Head {
            status: 206,
            content_length: None,
            content_range: Some("bytes 1-18446744073709551615/*".into()),
            content_type: Some("audio/flac".into()),
            ..Head::default()
        };
        let sh = stream_head(&max_valid, 1).unwrap();
        assert_eq!((sh.start, sh.range_honoured, sh.length, sh.total), (1, true, Some(u64::MAX), None));

        // Normal range without Content-Length derives length from Content-Range.
        let normal = Head {
            status: 206,
            content_length: None,
            content_range: Some("bytes 10-19/100".into()),
            content_type: Some("audio/flac".into()),
            ..Head::default()
        };
        let sh = stream_head(&normal, 10).unwrap();
        assert_eq!((sh.start, sh.range_honoured, sh.length, sh.total), (10, true, Some(10), Some(100)));
    }
}
