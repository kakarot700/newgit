//! Minimal, total HTTP/1.1 request parser + response writer (std only).
//!
//! Deliberately small surface: request-line + headers + Content-Length body,
//! every size capped, no chunked encoding (v1 clients always send
//! Content-Length; a chunked request gets 501), no keep-alive pipelining
//! (responses always close). Malformed anything ⇒ `Err((status, message))`
//! with a JSON envelope the router turns into a response — the parser never
//! panics, never allocates beyond caps (fuzzed in tests/fuzz_parsers.rs).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use crate::error::{Error, Result};
use crate::remote::proto::HDR_PROTOCOL;

pub const MAX_REQUEST_LINE: usize = 16 * 1024;
pub const MAX_HEADER_COUNT: usize = 128;
pub const MAX_HEADER_TOTAL: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// Path without query string, percent-decoded.
    pub path: String,
    /// Percent-decoded query pairs, in order.
    pub query: Vec<(String, String)>,
    /// Header keys lowercased; duplicate singleton fields are rejected, while
    /// unrelated extension headers retain last-value-wins behavior.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }
    pub fn query_get(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse error carrying the HTTP status the server should answer with.
pub type HttpError = (u16, String);

fn err(status: u16, msg: impl Into<String>) -> HttpError {
    (status, msg.into())
}

fn read_error_status(error: &std::io::Error) -> u16 {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => 408,
        _ => 400,
    }
}

/// Read one request. `max_body` caps Content-Length AND the actual read.
pub fn read_request<R: BufRead>(
    r: &mut R,
    max_body: u64,
) -> std::result::Result<Request, HttpError> {
    let line = read_bounded_line(r, MAX_REQUEST_LINE)
        .map_err(|e| err(read_error_status(&e), format!("request line read: {e}")))?;
    let Some(line) = line else {
        return Err(err(400, "empty request (connection closed)"));
    };
    let line = line.trim_end();
    let mut parts = line.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("");
    if method.is_empty() || target.is_empty() {
        return Err(err(400, "malformed request line"));
    }
    if !matches!(method.as_str(), "GET" | "POST" | "HEAD" | "PUT" | "DELETE") {
        return Err(err(405, format!("unsupported method {method}")));
    }
    if !version.starts_with("HTTP/1.") {
        return Err(err(
            400,
            format!("unsupported version {version:?} (HTTP/1.x only)"),
        ));
    }
    if !target.starts_with('/') || target.contains("://") {
        // absolute-form URIs and authority-form are out of scope for v1
        return Err(err(
            400,
            "target must be an origin-form path starting with /",
        ));
    }

    let mut headers = BTreeMap::new();
    let mut header_count = 0usize;
    let mut header_total = 0usize;
    loop {
        let remaining = MAX_HEADER_TOTAL.saturating_sub(header_total);
        let hl = read_bounded_line(r, remaining)
            .map_err(|e| err(read_error_status(&e), format!("header read: {e}")))?;
        let Some(hl) = hl else {
            return Err(err(400, "connection closed inside headers"));
        };
        header_total += hl.len();
        if header_total > MAX_HEADER_TOTAL {
            return Err(err(400, "too many/too large headers"));
        }
        let hl = hl.trim_end();
        if hl.is_empty() {
            break;
        }
        header_count += 1;
        if header_count > MAX_HEADER_COUNT {
            return Err(err(400, "too many/too large headers"));
        }
        let (k, v) = hl
            .split_once(':')
            .ok_or_else(|| err(400, format!("malformed header {hl:?}")))?;
        if k.is_empty() || !k.bytes().all(is_field_name_byte) {
            return Err(err(400, format!("malformed header name {k:?}")));
        }
        let k = k.to_ascii_lowercase();
        if is_singleton_header(&k) && headers.contains_key(&k) {
            return Err(err(400, format!("duplicate singleton HTTP header {k:?}")));
        }
        headers.insert(k, v.trim().to_string());
    }

    if headers.contains_key("transfer-encoding") && headers.contains_key("content-length") {
        return Err(err(
            400,
            "Content-Length and Transfer-Encoding must not appear together",
        ));
    }

    let body = if headers
        .get("transfer-encoding")
        .map(|v| !v.eq_ignore_ascii_case("identity"))
        .unwrap_or(false)
    {
        return Err(err(
            501,
            "chunked/encoded transfer not supported (v1 uses Content-Length)",
        ));
    } else {
        match headers.get("content-length") {
            None => Vec::new(),
            Some(cl) => {
                let cl = cl.trim();
                if cl.is_empty() || !cl.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(err(400, format!("bad Content-Length {cl:?}")));
                }
                let len: u64 = cl
                    .parse()
                    .map_err(|_| err(400, format!("bad Content-Length {cl:?}")))?;
                if len > max_body {
                    return Err(err(413, format!("body {len} exceeds limit {max_body}")));
                }
                let mut buf = vec![0u8; len as usize];
                read_exact(r, &mut buf)
                    .map_err(|e| err(read_error_status(&e), format!("body read: {e}")))?;
                buf
            }
        }
    };

    let (path, query) = split_target(&target)?;
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Read one line without allowing the input to grow the destination beyond its
/// configured wire-byte limit. `read_line` allocates until a newline arrives,
/// so it cannot enforce a limit that is checked only after the call returns.
fn read_bounded_line<R: BufRead>(r: &mut R, max_len: usize) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(max_len.min(4096));
    loop {
        let next = {
            let available = r.fill_buf()?;
            if available.is_empty() {
                None
            } else {
                let remaining = max_len.saturating_sub(bytes.len());
                let inspect_len = available.len().min(remaining.saturating_add(1));
                let newline = available[..inspect_len]
                    .iter()
                    .position(|byte| *byte == b'\n');
                let (line_len, terminated) = match newline {
                    Some(index) => (index + 1, true),
                    None if available.len() <= remaining => (available.len(), false),
                    None => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "HTTP line exceeds its configured byte limit",
                        ));
                    }
                };
                if line_len > remaining {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "HTTP line exceeds its configured byte limit",
                    ));
                }
                bytes.extend_from_slice(&available[..line_len]);
                Some((line_len, terminated))
            }
        };
        let Some((consumed, terminated)) = next else {
            break;
        };
        r.consume(consumed);
        if terminated {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "HTTP line is not UTF-8"))
}

fn is_field_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn is_singleton_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "content-length"
            | "content-type"
            | "git-protocol"
            | "host"
            | "transfer-encoding"
    ) || name == HDR_PROTOCOL
}

fn read_exact<R: BufRead>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed inside body",
            ));
        }
        filled += n;
    }
    Ok(())
}

/// Split `"/path?query"` and percent-decode both halves. Invalid percent
/// escapes decode to the raw bytes (total function, never fails).
fn split_target(target: &str) -> std::result::Result<(String, Vec<(String, String)>), HttpError> {
    if target.len() > MAX_REQUEST_LINE {
        return Err(err(400, "target too long"));
    }
    let (p, q) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let path = percent_decode(p);
    if path.is_empty() || !path.starts_with('/') || path.contains('\0') {
        return Err(err(400, "invalid path after decoding"));
    }
    let mut query = Vec::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        query.push((percent_decode(k), percent_decode(v)));
    }
    Ok((path, query))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hi = (b[i + 1] as char).to_digit(16);
                let lo = (b[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// Write a JSON (or plain) response and signal connection close.
pub fn write_response<W: Write>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{HDR_PROTOCOL}: {}\r\nConnection: close\r\n\r\n",
        status_text(status),
        body.len(),
        crate::remote::proto::PROTOCOL_VERSION
    );
    w.write_all(head.as_bytes())
        .and_then(|_| w.write_all(body))
        .and_then(|_| w.flush())
        .map_err(Error::from)?;
    Ok(())
}

/// Write an unwrapped Git smart-HTTP response. Git packet-line and packfile
/// bytes are binary protocol data, not NewGit JSON envelopes.
pub fn write_git_response<W: Write>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache, max-age=0, must-revalidate\r\nPragma: no-cache\r\nExpires: Fri, 01 Jan 1980 00:00:00 GMT\r\nConnection: close\r\n\r\n",
        status_text(status),
        body.len(),
    );
    w.write_all(head.as_bytes())
        .and_then(|_| w.write_all(body))
        .and_then(|_| w.flush())
        .map_err(Error::from)?;
    Ok(())
}

/// Map a NewGit error to an HTTP status (single source of truth for the
/// router AND the client's reverse mapping).
pub fn status_for_error(e: &Error) -> u16 {
    match e {
        Error::Io { source, .. } if source.kind() == std::io::ErrorKind::TimedOut => 504,
        Error::Auth(_) => 401,
        Error::Forbidden(_) => 403,
        Error::CasFailed(_) | Error::LockBusy(_) | Error::Conflict(_) => 409,
        Error::Limit(_) => 413,
        Error::NotFound(_) | Error::RefNotFound(_) | Error::NotRepo(_) => 404,
        Error::Invalid(_) | Error::Malformed(_) | Error::InvalidRef(_) | Error::Protocol(_) => 400,
        Error::Bug(_) => 500,
        _ => 500,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_bytes(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    fn parse(s: &str, max_body: u64) -> std::result::Result<Request, HttpError> {
        let b = req_bytes(s);
        read_request(&mut b.as_slice(), max_body)
    }

    #[test]
    fn parses_full_request() {
        let r = parse(
            "POST /v1/objects/put?x=1&y=a%20b HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nAuthorization: Bearer t\r\n\r\nhello",
            1024,
        )
        .unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/v1/objects/put");
        assert_eq!(
            r.query,
            vec![("x".into(), "1".into()), ("y".into(), "a b".into())]
        );
        assert_eq!(r.header("authorization"), Some("Bearer t"));
        assert_eq!(r.body, b"hello");
    }

    #[test]
    fn get_without_content_length_has_empty_body() {
        let r = parse("GET /v1/info HTTP/1.1\r\nHost: h\r\n\r\n", 1024).unwrap();
        assert!(r.body.is_empty());
        assert_eq!(r.path, "/v1/info");
    }

    #[test]
    fn rejects_body_over_limit_with_413() {
        let e = parse("POST /v1/x HTTP/1.1\r\nContent-Length: 2000\r\n\r\n", 1024).unwrap_err();
        assert_eq!(e.0, 413);
    }

    #[test]
    fn rejects_bad_content_length_and_truncated_body() {
        assert_eq!(
            parse("POST /v1/x HTTP/1.1\r\nContent-Length: abc\r\n\r\n", 1024)
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(
            parse("POST /v1/x HTTP/1.1\r\nContent-Length: +0\r\n\r\n", 1024)
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(
            parse(
                "POST /v1/x HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort",
                1024
            )
            .unwrap_err()
            .0,
            400
        );
    }

    #[test]
    fn rejects_chunked_with_501_and_bad_version_with_400() {
        assert_eq!(
            parse(
                "POST /v1/x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                1024
            )
            .unwrap_err()
            .0,
            501
        );
        assert_eq!(parse("GET / HTTP/2.0\r\n\r\n", 1024).unwrap_err().0, 400);
    }

    #[test]
    fn rejects_oversized_request_and_header_lines_with_bounded_reads() {
        let request_line = format!("GET /{} HTTP/1.1\r\n\r\n", "x".repeat(MAX_REQUEST_LINE + 1));
        assert_eq!(parse(&request_line, 1024).unwrap_err().0, 400);

        let header_line = format!(
            "GET / HTTP/1.1\r\nX-Test: {}\r\n\r\n",
            "x".repeat(MAX_HEADER_TOTAL + 1)
        );
        assert_eq!(parse(&header_line, 1024).unwrap_err().0, 400);
    }

    #[test]
    fn counts_duplicate_header_lines_toward_the_header_limit() {
        let mut request = String::from("GET / HTTP/1.1\r\n");
        for _ in 0..=MAX_HEADER_COUNT {
            request.push_str("X-Test: value\r\n");
        }
        request.push_str("\r\n");
        assert_eq!(parse(&request, 1024).unwrap_err().0, 400);
    }

    #[test]
    fn rejects_ambiguous_duplicate_singletons_and_body_framing() {
        for request in [
            "POST /x HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            "POST /x HTTP/1.1\r\nTransfer-Encoding: identity\r\nTransfer-Encoding: chunked\r\n\r\n",
            "POST /x HTTP/1.1\r\nContent-Length: 0\r\nTransfer-Encoding: identity\r\n\r\n",
            "GET /x HTTP/1.1\r\nAuthorization: Bearer a\r\nAuthorization: Bearer b\r\n\r\n",
            "GET /x HTTP/1.1\r\nGit-Protocol: version=0\r\nGit-Protocol: version=1\r\n\r\n",
            "GET /x HTTP/1.1\r\nX-NewGit-Protocol: 1\r\nX-NewGit-Protocol: 2\r\n\r\n",
        ] {
            assert_eq!(parse(request, 1024).unwrap_err().0, 400, "{request:?}");
        }
    }

    #[test]
    fn rejects_whitespace_in_http_field_names_but_keeps_extension_last_wins() {
        for header in [
            " Content-Length: 0",
            "Content-Length : 0",
            "Content-Length\t: 0",
        ] {
            let request = format!("POST /x HTTP/1.1\r\n{header}\r\n\r\n");
            assert_eq!(parse(&request, 1024).unwrap_err().0, 400, "{header:?}");
        }

        let request = "GET /x HTTP/1.1\r\nX-Extra: first\r\nX-Extra: second\r\n\r\n";
        assert_eq!(
            parse(request, 1024).unwrap().header("x-extra"),
            Some("second")
        );
    }

    #[test]
    fn authenticated_authorization_denials_are_forbidden_not_unauthorized() {
        assert_eq!(status_for_error(&Error::Auth("invalid token".into())), 401);
        assert_eq!(
            status_for_error(&Error::Forbidden("admin required".into())),
            403
        );
        assert_eq!(Error::Forbidden("admin required".into()).category(), "auth");
        assert_eq!(Error::Forbidden("admin required".into()).exit_code(), 7);
    }

    #[test]
    fn rejects_malformed_pieces() {
        for (input, status) in [
            ("", 400),                                     // empty
            ("garbage\r\n\r\n", 400),                      // no spaces
            ("GET /x HTTP/1.1\r\nbadheader\r\n\r\n", 400), // header without colon
            ("TRACE /x HTTP/1.1\r\n\r\n", 405),            // unsupported method
            ("GET http://evil/x HTTP/1.1\r\n\r\n", 400),   // absolute form
            ("GET /a%00b HTTP/1.1\r\n\r\n", 400),          // NUL after decode
            ("GET /x HTTP/1.1\r\n\r\nextra", 200),         // trailing bytes ignored (next request)
        ] {
            let r = parse(input, 1024);
            if status == 200 {
                assert!(r.is_ok(), "{input:?} should parse");
            } else {
                assert_eq!(r.unwrap_err().0, status, "{input:?}");
            }
        }
    }

    #[test]
    fn percent_decode_is_total() {
        assert_eq!(percent_decode("a%2Fb%zz%2"), "a/b%zz%2");
        assert_eq!(percent_decode("%41+%42"), "A B");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}"); // invalid utf8 → lossy replacement
    }

    #[test]
    fn response_framing_is_exact() {
        let mut out = Vec::new();
        write_response(&mut out, 404, "application/json", b"{\"ok\":false}").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("Content-Length: 12\r\n"));
        assert!(s.contains(&format!("{HDR_PROTOCOL}: 1\r\n")));
        assert!(s.ends_with("\r\n\r\n{\"ok\":false}"));
    }

    #[test]
    fn request_read_timeouts_map_to_408() {
        let error = std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline");
        assert_eq!(read_error_status(&error), 408);
        assert_eq!(status_text(408), "Request Timeout");
    }

    #[test]
    fn timed_out_git_work_maps_to_gateway_timeout() {
        let error = Error::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "operation timed out",
        ));
        assert_eq!(status_for_error(&error), 504);
        assert_eq!(status_text(504), "Gateway Timeout");
    }
}
