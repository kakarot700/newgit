//! NewGit remote server — HTTP/1.1 + JSON protocol v1 (std only, D-002).
//!
//! Thread-per-connection with a bounded pool; every connection reopens the
//! repo (cheap: recovery is idempotent, the txn lock serializes ref writes).
//! Auth is bearer-token (SHA-256 at rest, roles read<write<admin); every
//! request is audit-logged. All limits are enforced BEFORE allocation
//! (Content-Length cap, batch cap, body cap). No TLS in v1 — run behind a
//! reverse proxy for encryption (documented in docs/PROTOCOL.md).

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, BufWriter, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::object::envelope;
use crate::object::types::Object;
use crate::object::ObjectId;
use crate::ops::verify;
use crate::remote::audit::AuditLog;
use crate::remote::auth::{self, Principal, Role, TokenFile};
use crate::remote::git_http;
use crate::remote::git_receive;
use crate::remote::http::{self, Request};
use crate::remote::negotiate;
use crate::remote::proto::*;
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::util::base64;

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(300);
const ACCEPT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// `host:port` (port 0 ⇒ ephemeral; actual port via handle.addr()).
    pub bind: String,
    pub repo_root: PathBuf,
    /// JSON token file (see remote::auth). Missing/empty ⇒ no tokens.
    pub token_file: PathBuf,
    /// Allow unauthenticated READ endpoints (info is always anonymous).
    pub allow_anonymous_read: bool,
    /// Hard cap on request bodies (pre-read) and buffered Git smart-HTTP responses.
    pub max_body: u64,
    /// Max concurrent connection threads; beyond ⇒ 429.
    pub max_threads: usize,
    /// Serve the embedded Web UI at `/` (static HTML, no auth — it contains
    /// no data; every data endpoint still enforces roles).
    pub ui: bool,
    /// Exact Git ref names that only admin-role tokens may change via receive-pack.
    pub protected_refs: HashSet<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: DEFAULT_BIND.into(),
            repo_root: PathBuf::from("."),
            token_file: PathBuf::from(".newgit/tokens.json"),
            allow_anonymous_read: false,
            max_body: 64 * 1024 * 1024,
            max_threads: 32,
            ui: false,
            protected_refs: HashSet::new(),
        }
    }
}

/// A running server (used by tests and by `newgit serve`).
#[derive(Debug)]
pub struct ServerHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// Signal the accept loop to stop and wait for it. Connection workers are
    /// detached; each socket I/O operation uses `IO_TIMEOUT`, complete request
    /// reads have a five-minute accept-start deadline, and Git projection/pack
    /// work remains bounded by the adapter's 120-second child-process deadline.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Bind and spawn the accept loop; returns immediately.
pub fn spawn(cfg: ServerConfig) -> Result<ServerHandle> {
    for ref_name in &cfg.protected_refs {
        git_receive::validate_protected_ref(ref_name)?;
    }
    let listener = TcpListener::bind(&cfg.bind)
        .map_err(|e| Error::Config(format!("cannot bind {}: {e}", cfg.bind)))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error::Bug(format!("local_addr: {e}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::Bug(format!("set_nonblocking: {e}")))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let join = std::thread::Builder::new()
        .name("newgit-serve-accept".into())
        .spawn(move || accept_loop(listener, cfg, stop2))
        .map_err(|e| Error::Bug(format!("spawn accept loop: {e}")))?;
    Ok(ServerHandle {
        addr,
        stop,
        join: Some(join),
    })
}

fn accept_loop(listener: TcpListener, cfg: ServerConfig, stop: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let request_deadline = Instant::now() + REQUEST_READ_TIMEOUT;
                // Nonblocking listeners can yield nonblocking accepted sockets on Windows;
                // restore blocking mode so the per-socket read timeout is effective.
                let stream = match set_accepted_stream_blocking(stream) {
                    Ok(stream) => stream,
                    Err(_) => continue,
                };
                if active.load(Ordering::Relaxed) >= cfg.max_threads {
                    let mut w = BufWriter::new(stream);
                    let body = envelope_err(429, "limit", "server busy: too many connections");
                    let _ = http::write_response(&mut w, 429, "application/json", &body);
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let cfg = cfg.clone();
                let active_conn = active.clone();
                let res = std::thread::Builder::new()
                    .name("newgit-serve-conn".into())
                    .spawn(move || {
                        handle_conn(stream, &cfg, request_deadline);
                        active_conn.fetch_sub(1, Ordering::Relaxed);
                    });
                if res.is_err() {
                    active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break, // listener dead
        }
    }
}

fn set_accepted_stream_blocking(stream: TcpStream) -> io::Result<TcpStream> {
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn envelope_ok(data: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": true, "data": data })).unwrap_or_default()
}

fn envelope_err(status_hint: u16, category: &str, message: &str) -> Vec<u8> {
    let _ = status_hint;
    serde_json::to_vec(&json!({
        "ok": false,
        "error": { "category": category, "message": message }
    }))
    .unwrap_or_default()
}

/// BufRead adapter that applies both the existing idle timeout and one
/// accept-start deadline to every socket read, including Content-Length bodies.
struct DeadlineBufReader {
    inner: BufReader<TcpStream>,
    deadline: Instant,
}

impl DeadlineBufReader {
    fn new(stream: TcpStream, deadline: Instant) -> Self {
        Self {
            inner: BufReader::new(stream),
            deadline,
        }
    }

    fn arm_read_timeout(&self) -> io::Result<()> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP request receive deadline expired",
            ));
        }
        self.inner
            .get_ref()
            .set_read_timeout(Some(remaining.min(IO_TIMEOUT)))
    }
}

impl Read for DeadlineBufReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.arm_read_timeout()?;
        self.inner.read(buf)
    }
}

impl BufRead for DeadlineBufReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.arm_read_timeout()?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

fn handle_conn(stream: TcpStream, cfg: &ServerConfig, request_deadline: Instant) {
    #[cfg(test)]
    let connection_started = std::time::Instant::now();
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    #[cfg(feature = "smart-http-diagnostics")]
    let _diagnostic_scope = crate::remote::diagnostics::begin(
        &peer,
        IO_TIMEOUT.as_millis() as u64,
        REQUEST_READ_TIMEOUT.as_millis() as u64,
        request_deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64,
    );
    let out_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut r = DeadlineBufReader::new(stream, request_deadline);
    let mut w = BufWriter::new(out_stream);

    // Repo + tokens are loaded per connection (cheap, always fresh).
    #[cfg(test)]
    let repo_open_started = std::time::Instant::now();
    let repo = match Repo::open(&cfg.repo_root) {
        Ok(repo) => repo,
        Err(e) => {
            let body = envelope_err(500, e.category(), &e.to_string());
            let _ = http::write_response(&mut w, 500, "application/json", &body);
            return;
        }
    };
    #[cfg(test)]
    crate::remote::bench_timing::record("server.repo_open", repo_open_started.elapsed());
    let tokens = auth::load(&cfg.token_file).unwrap_or_default();
    let audit = AuditLog::new(repo.ng());

    #[cfg(any(test, feature = "smart-http-diagnostics"))]
    let request_read_started = std::time::Instant::now();
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "request_read_start",
        &[
            (
                "deadline_remaining_ms",
                json!(request_deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64),
            ),
            (
                "socket_idle_timeout_ms",
                json!(IO_TIMEOUT.as_millis() as u64),
            ),
        ],
    );
    let req = match http::read_request(&mut r, cfg.max_body) {
        Ok(req) => req,
        Err((status, msg)) => {
            #[cfg(feature = "smart-http-diagnostics")]
            let deadline_remaining_ms = request_deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event(
                "request_read_end",
                &[
                    ("http_status", json!(status)),
                    ("deadline_remaining_ms", json!(deadline_remaining_ms)),
                    (
                        "timeout_reason",
                        json!(if status == 408 {
                            if deadline_remaining_ms == 0 {
                                "absolute_receive_deadline"
                            } else {
                                "socket_timeout_or_would_block_before_deadline"
                            }
                        } else {
                            "request_not_fully_read"
                        }),
                    ),
                    (
                        "read_duration_ms",
                        json!(request_read_started.elapsed().as_millis() as u64),
                    ),
                ],
            );
            let category = if status == 413 { "limit" } else { "protocol" };
            audit.append("pre-auth", "?", &msg, status, Some(category));
            let body = envelope_err(status, category, &msg);
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event(
                "response_start",
                &[
                    ("http_status", json!(status)),
                    ("response_bytes", json!(body.len())),
                ],
            );
            #[cfg(feature = "smart-http-diagnostics")]
            let response_started = Instant::now();
            let response_result = http::write_response(&mut w, status, "application/json", &body);
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event(
                "response_end",
                &[
                    ("http_status", json!(status)),
                    (
                        "response_duration_ms",
                        json!(response_started.elapsed().as_millis() as u64),
                    ),
                    (
                        "connection_state",
                        json!(diagnostic_connection_state(&response_result)),
                    ),
                    (
                        "connection_error_kind",
                        json!(diagnostic_connection_error_kind(&response_result)),
                    ),
                ],
            );
            #[cfg(not(feature = "smart-http-diagnostics"))]
            drop(response_result);
            return;
        }
    };
    #[cfg(test)]
    crate::remote::bench_timing::record("server.request_read", request_read_started.elapsed());
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "request_read_end",
        &[
            ("parsed", json!(true)),
            ("request_body_bytes", json!(req.body.len())),
            (
                "deadline_remaining_ms",
                json!(request_deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64),
            ),
            (
                "read_duration_ms",
                json!(request_read_started.elapsed().as_millis() as u64),
            ),
        ],
    );
    crate::obs::event(
        "remote_request",
        &[
            ("peer", json!(peer)),
            ("method", json!(req.method)),
            ("path", json!(req.path)),
        ],
    );

    // Static UI first (no auth: the HTML contains zero data; every data
    // endpoint behind it enforces roles with the user's own token).
    #[cfg(any(test, feature = "smart-http-diagnostics"))]
    let route_started = std::time::Instant::now();
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "route_start",
        &[("operation", json!(diagnostic_operation(&req)))],
    );
    let (status, body, ctype, principal_id, category) =
        if cfg.ui && matches!(req.path.as_str(), "/" | "/index.html") && req.method == "GET" {
            (
                200,
                crate::ui::INDEX_HTML.as_bytes().to_vec(),
                "text/html; charset=utf-8",
                "ui-static".to_string(),
                None,
            )
        } else if matches!(
            req.path.as_str(),
            "/info/refs" | "/git-upload-pack" | "/git-receive-pack"
        ) {
            route_git_http(&repo, &tokens, cfg, &req)
        } else {
            let (st, bd, who, cat) = route(&repo, &tokens, cfg, &req);
            (st, bd, "application/json", who, cat)
        };
    #[cfg(test)]
    crate::remote::bench_timing::record(
        format!("server.route {} {}", req.method, req.path),
        route_started.elapsed(),
    );
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "route_end",
        &[
            ("operation", json!(diagnostic_operation(&req))),
            ("http_status", json!(status)),
            ("response_body_bytes", json!(body.len())),
            (
                "route_duration_ms",
                json!(route_started.elapsed().as_millis() as u64),
            ),
            (
                "timeout_reason",
                json!(if status == 504 {
                    "git_operation_deadline"
                } else {
                    "none"
                }),
            ),
        ],
    );
    #[cfg(test)]
    let audit_started = std::time::Instant::now();
    audit.append(
        &principal_id,
        &req.method,
        &req.path,
        status,
        category.as_deref(),
    );
    #[cfg(test)]
    crate::remote::bench_timing::record("server.audit", audit_started.elapsed());
    #[cfg(any(test, feature = "smart-http-diagnostics"))]
    let response_write_started = std::time::Instant::now();
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "response_start",
        &[
            ("http_status", json!(status)),
            ("response_bytes", json!(body.len())),
        ],
    );
    let response_result = if ctype.starts_with("application/x-git-") {
        http::write_git_response(&mut w, status, ctype, &body)
    } else {
        http::write_response(&mut w, status, ctype, &body)
    };
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "response_end",
        &[
            ("http_status", json!(status)),
            (
                "response_duration_ms",
                json!(response_write_started.elapsed().as_millis() as u64),
            ),
            (
                "connection_state",
                json!(diagnostic_connection_state(&response_result)),
            ),
            (
                "connection_error_kind",
                json!(diagnostic_connection_error_kind(&response_result)),
            ),
        ],
    );
    #[cfg(not(feature = "smart-http-diagnostics"))]
    drop(response_result);
    #[cfg(test)]
    crate::remote::bench_timing::record("server.response_write", response_write_started.elapsed());
    #[cfg(test)]
    crate::remote::bench_timing::record(
        format!("server.total {} {}", req.method, req.path),
        connection_started.elapsed(),
    );
}

#[cfg(feature = "smart-http-diagnostics")]
fn diagnostic_operation(req: &Request) -> &'static str {
    match (
        req.method.as_str(),
        req.path.as_str(),
        req.query_get("service"),
    ) {
        ("GET", "/info/refs", Some("git-upload-pack")) => "advertise_upload_pack",
        ("GET", "/info/refs", Some("git-receive-pack")) => "advertise_receive_pack",
        ("POST", "/git-upload-pack", _) => "upload_pack",
        ("POST", "/git-receive-pack", _) => "receive_pack",
        _ => "other",
    }
}

#[cfg(feature = "smart-http-diagnostics")]
fn diagnostic_connection_state(result: &Result<()>) -> &'static str {
    match result {
        Ok(()) => "response_flushed",
        Err(Error::Io { source, .. })
            if matches!(
                source.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            "peer_closed_or_reset"
        }
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::TimedOut => {
            "response_write_timeout"
        }
        Err(_) => "response_write_error",
    }
}

#[cfg(feature = "smart-http-diagnostics")]
fn diagnostic_connection_error_kind(result: &Result<()>) -> Option<String> {
    match result {
        Err(Error::Io { source, .. }) => Some(format!("{:?}", source.kind())),
        _ => None,
    }
}

/// Smart-HTTP compatibility boundary. Read and write services share auth but
/// use separate adapters; receive-pack exposes the narrow transactional
/// branch create/update/delete slice implemented by `git_receive`.
fn route_git_http(
    repo: &Repo,
    tokens: &TokenFile,
    cfg: &ServerConfig,
    req: &Request,
) -> (u16, Vec<u8>, &'static str, String, Option<String>) {
    let principal = match tokens.authenticate(req.header("authorization")) {
        Ok(p) => p,
        Err(e) => {
            let status = http::status_for_error(&e);
            return (
                status,
                envelope_err(status, e.category(), &e.to_string()),
                "application/json",
                "bad-token".into(),
                Some(e.category().to_string()),
            );
        }
    };
    let who = principal
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "anonymous".into());
    let role = principal.as_ref().map(|p| p.role).unwrap_or(Role::Read);
    let receive_pack = req.path == "/git-receive-pack"
        || (req.path == "/info/refs"
            && req.query.len() == 1
            && req.query[0].0 == "service"
            && req.query[0].1 == "git-receive-pack");
    let required_role = if receive_pack {
        Role::Write
    } else {
        Role::Read
    };
    if (receive_pack || !cfg.allow_anonymous_read)
        && !auth::authorize(principal.as_ref(), required_role)
    {
        let status = if principal.is_none() { 401 } else { 403 };
        return (
            status,
            envelope_err(
                status,
                "auth",
                if receive_pack {
                    "Git receive-pack requires write access"
                } else {
                    "Git upload-pack requires read access"
                },
            ),
            "application/json",
            who,
            Some("auth".into()),
        );
    }

    let response = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/info/refs") => {
            if req.query.len() != 1 || req.query[0].0 != "service" {
                Err((
                    400,
                    "protocol",
                    "expected exactly one service query parameter".to_string(),
                ))
            } else if req.query[0].1 == "git-receive-pack" {
                git_receive::validate_git_protocol(req.header("git-protocol"))
                    .and_then(|protocol| git_receive::advertise(repo, protocol, cfg.max_body))
                    .map(|body| ("application/x-git-receive-pack-advertisement", body))
                    .map_err(|e| (http::status_for_error(&e), e.category(), e.to_string()))
            } else if req.query[0].1 != "git-upload-pack" {
                Err((
                    403,
                    "protocol",
                    "requested Git service is not supported".to_string(),
                ))
            } else {
                match git_http::validate_git_protocol(req.header("git-protocol")) {
                    Ok(protocol) => git_http::advertise(repo, protocol, cfg.max_body)
                        .map(|body| ("application/x-git-upload-pack-advertisement", body))
                        .map_err(|e| (http::status_for_error(&e), e.category(), e.to_string())),
                    Err(e) => Err((http::status_for_error(&e), e.category(), e.to_string())),
                }
            }
        }
        ("POST", "/git-upload-pack") => {
            let content_type = req
                .header("content-type")
                .and_then(|v| v.split(';').next())
                .map(str::trim);
            if !content_type
                .is_some_and(|v| v.eq_ignore_ascii_case("application/x-git-upload-pack-request"))
            {
                Err((
                    400,
                    "protocol",
                    "expected application/x-git-upload-pack-request".to_string(),
                ))
            } else {
                match git_http::validate_git_protocol(req.header("git-protocol")) {
                    Ok(protocol) => git_http::upload_pack(repo, protocol, &req.body, cfg.max_body)
                        .map(|body| ("application/x-git-upload-pack-result", body))
                        .map_err(|e| (http::status_for_error(&e), e.category(), e.to_string())),
                    Err(e) => Err((http::status_for_error(&e), e.category(), e.to_string())),
                }
            }
        }
        ("POST", "/git-receive-pack") => {
            let content_type = req
                .header("content-type")
                .and_then(|v| v.split(';').next())
                .map(str::trim);
            if !content_type
                .is_some_and(|v| v.eq_ignore_ascii_case("application/x-git-receive-pack-request"))
            {
                Err((
                    400,
                    "protocol",
                    "expected application/x-git-receive-pack-request".to_string(),
                ))
            } else {
                match git_receive::validate_git_protocol(req.header("git-protocol")) {
                    Ok(protocol) => git_receive::receive_pack(
                        repo,
                        &req.body,
                        protocol,
                        cfg.max_body,
                        &who,
                        role,
                        &cfg.protected_refs,
                    )
                    .map(|body| ("application/x-git-receive-pack-result", body))
                    .map_err(|e| (http::status_for_error(&e), e.category(), e.to_string())),
                    Err(e) => Err((http::status_for_error(&e), e.category(), e.to_string())),
                }
            }
        }
        (_, "/git-receive-pack") => Err((405, "protocol", "method not allowed".to_string())),
        ("GET", "/git-upload-pack") | ("POST", "/info/refs") => Err((
            405,
            "protocol",
            "method not allowed for Git smart-HTTP endpoint".to_string(),
        )),
        _ => Err((
            404,
            "protocol",
            "unknown Git smart-HTTP endpoint".to_string(),
        )),
    };

    match response {
        Ok((content_type, body)) => (200, body, content_type, who, None),
        Err((status, category, message)) => (
            status,
            envelope_err(status, category, &message),
            "application/json",
            who,
            Some(category.to_string()),
        ),
    }
}

/// Wire-spec guard (iteration 12 audit): diff specs may be ref names or
/// oids, but the internal namespaces (`workspaces/*`, `chains/*`) and the
/// local-only `ws:<name>` shorthand NEVER cross the wire — same invariant
/// as /v1/refs listings and refs/update (THREAT_MODEL §E). Without this,
/// a read-role (or anonymous-read) client could probe workspace names and
/// positions through /v1/diff.
fn check_wire_spec(spec: &str) -> Result<()> {
    if spec.starts_with("ws:") || crate::remote::proto::is_internal_ref(spec) {
        return Err(Error::Invalid(format!(
            "spec {spec:?}: internal namespaces and the ws: shorthand are local-only (not exposed remotely)"
        )));
    }
    Ok(())
}

/// Route one request. Returns (status, body, principal-id, error-category).
fn route(
    repo: &Repo,
    tokens: &TokenFile,
    cfg: &ServerConfig,
    req: &Request,
) -> (u16, Vec<u8>, String, Option<String>) {
    // Authenticate once; a bad token is a hard 401 for EVERY endpoint
    // (including anonymous ones — presenting invalid credentials is an
    // error, not a downgrade).
    let principal = match tokens.authenticate(req.header("authorization")) {
        Ok(p) => p,
        Err(e) => {
            return (
                http::status_for_error(&e),
                envelope_err(401, e.category(), &e.to_string()),
                "bad-token".into(),
                Some(e.category().to_string()),
            )
        }
    };
    let who = principal
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "anonymous".into());

    let known_paths: &[&str] = &[
        "/healthz",
        "/v1/info",
        "/v1/refs",
        "/v1/have",
        "/v1/negotiate",
        "/v1/objects/get",
        "/v1/objects/put",
        "/v1/refs/update",
        "/v1/audit",
        "/v1/object",
        "/v1/diff",
        "/v1/goals",
        "/v1/changes",
        "/v1/proposals",
    ];
    if !known_paths.contains(&req.path.as_str()) {
        return (
            404,
            envelope_err(404, "protocol", &format!("unknown endpoint {}", req.path)),
            who,
            Some("protocol".into()),
        );
    }

    // Per-endpoint (method, role) policy.
    let policy: (&str, Role) = match req.path.as_str() {
        "/healthz" => ("GET", Role::Read), // anonymous allowed below
        "/v1/info" => ("GET", Role::Read), // anonymous allowed below
        "/v1/refs" | "/v1/have" | "/v1/negotiate" | "/v1/objects/get" => ("", Role::Read),
        "/v1/goals" | "/v1/changes" | "/v1/proposals" => ("GET", Role::Read),
        "/v1/object" | "/v1/diff" => ("POST", Role::Read),
        "/v1/objects/put" | "/v1/refs/update" => ("POST", Role::Write),
        "/v1/audit" => ("GET", Role::Admin),
        _ => unreachable!("known_paths checked above"),
    };
    let anonymous_ok = matches!(req.path.as_str(), "/healthz" | "/v1/info")
        || (cfg.allow_anonymous_read && policy.1 == Role::Read);
    match (&req.method[..], principal.as_ref()) {
        ("GET", _) if policy.0.is_empty() || policy.0 == "GET" => {}
        ("POST", _) if policy.0.is_empty() || policy.0 == "POST" => {}
        _ => {
            return (
                405,
                envelope_err(
                    405,
                    "protocol",
                    &format!("{} not allowed on {}", req.method, req.path),
                ),
                who,
                Some("protocol".into()),
            )
        }
    }
    if !anonymous_ok && !auth::authorize(principal.as_ref(), policy.1) {
        let status = if principal.is_none() { 401 } else { 403 };
        return (
            status,
            envelope_err(
                status,
                "auth",
                &format!("{} requires role {:?} or higher", req.path, policy.1),
            ),
            who,
            Some("auth".into()),
        );
    }

    let result = dispatch(repo, cfg, req, principal.as_ref());
    match result {
        Ok(data) => (200, envelope_ok(data), who, None),
        Err(e) => {
            let status = http::status_for_error(&e);
            (
                status,
                envelope_err(status, e.category(), &e.to_string()),
                who,
                Some(e.category().to_string()),
            )
        }
    }
}

fn dispatch(
    repo: &Repo,
    cfg: &ServerConfig,
    req: &Request,
    principal: Option<&Principal>,
) -> Result<Value> {
    let limits = repo.limits();
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => Ok(json!("healthy")),
        ("GET", "/v1/info") => {
            let head = head_string(repo)?;
            Ok(serde_json::to_value(InfoData {
                product: "newgit".into(),
                version: crate::VERSION.into(),
                protocol: PROTOCOL_VERSION,
                head,
                capabilities: {
                    let mut caps = vec![
                        "have".into(),
                        "negotiate".into(),
                        "objects-get".into(),
                        "objects-put".into(),
                        "refs-update".into(),
                        "audit".into(),
                        "object".into(),
                        "diff".into(),
                        "goals".into(),
                        "changes".into(),
                        "proposals".into(),
                    ];
                    if cfg.ui {
                        caps.push("ui".into());
                    }
                    caps
                },
                limits: LimitsInfo {
                    max_batch_objects: limits.max_batch_objects,
                    max_request_bytes: cfg.max_body.min(limits.max_request_bytes),
                },
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("GET", "/v1/refs") => {
            let all = repo.refs.list(None)?;
            let refs: Vec<RefEntry> = all
                .into_iter()
                .filter(|(name, _)| !is_internal_ref(name))
                .map(|(name, oid)| RefEntry {
                    name,
                    oid: oid.to_hex(),
                })
                .collect();
            Ok(serde_json::to_value(RefsData {
                head: head_string(repo)?,
                refs,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/have") => {
            let r: HaveReq = body_json(req)?;
            let oids = parse_oid_batch(&r.oids, limits.max_batch_objects)?;
            let have: Vec<String> = oids
                .into_iter()
                .filter(|o| repo.objects.contains(o))
                .map(|o| o.to_hex())
                .collect();
            Ok(serde_json::to_value(HaveData { have }).map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/negotiate") => {
            let r: NegotiateReq = body_json(req)?;
            let batch_cap = limits.max_batch_objects.max(10_000);
            let have = parse_oid_batch(&r.have, batch_cap)?;
            let want = parse_oid_batch(&r.want, batch_cap)?;
            // Haves the server does not store are ignored (superset
            // semantics — the client may receive extra objects, never
            // fewer than needed).
            let have_stored: Vec<ObjectId> = have
                .into_iter()
                .filter(|o| repo.objects.contains(o))
                .collect();
            for w in &want {
                if !repo.objects.contains(w) {
                    return Err(Error::NotFound(*w));
                }
            }
            let exclude = negotiate::closure(repo, &have_stored);
            let exclude: HashSet<ObjectId> = exclude.into_iter().collect();
            let send = negotiate::post_order(repo, &want, &exclude)?;
            Ok(serde_json::to_value(NegotiateData {
                send: send.iter().map(|o| o.to_hex()).collect(),
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/objects/get") => {
            let r: ObjectsGetReq = body_json(req)?;
            let ids = parse_oid_batch(&r.ids, limits.max_batch_objects)?;
            let mut objects = Vec::with_capacity(ids.len());
            for oid in &ids {
                let raw =
                    std::fs::read(repo.objects.path_for(oid)).map_err(|_| Error::NotFound(*oid))?;
                objects.push(ObjectWire {
                    data_b64: base64::encode(&raw),
                });
            }
            Ok(serde_json::to_value(ObjectsData { objects })
                .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/objects/put") => {
            let r: ObjectsPutReq = body_json(req)?;
            if r.objects.len() > limits.max_batch_objects {
                return Err(Error::Limit(format!(
                    "batch of {} objects exceeds max_batch_objects {}",
                    r.objects.len(),
                    limits.max_batch_objects
                )));
            }
            let mut stored = Vec::new();
            for (i, obj) in r.objects.iter().enumerate() {
                let bytes = base64::decode(&obj.data_b64)
                    .map_err(|e| Error::Malformed(format!("object {i}: bad base64: {e}")))?;
                // Self-verifying envelope: digest + id checked here...
                let (canonical, oid) = envelope::verify(&bytes, limits.max_object_bytes)
                    .map_err(|e| Error::Malformed(format!("object {i}: {e}")))?;
                // ...and the link-closure invariant: dependencies must
                // already be stored (batches are dependency-ordered; an
                // earlier object in THIS batch counts as stored).
                let decoded = Object::from_canonical(&canonical)
                    .map_err(|e| Error::Malformed(format!("object {i}: {e}")))?;
                for link in verify::object_links(&decoded) {
                    if !repo.objects.contains(&link) {
                        return Err(Error::Protocol(format!(
                            "object {i} ({oid}): missing dependency {link} — \
                             send dependencies first"
                        )));
                    }
                }
                let got = repo.objects.put_canonical(&canonical)?;
                if got != oid {
                    return Err(Error::Bug(format!("object {i}: put_canonical id mismatch")));
                }
                stored.push(oid.to_hex());
            }
            Ok(serde_json::to_value(PutData {
                stored: stored.len(),
                oids: stored,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/refs/update") => {
            let r: RefsUpdateReq = body_json(req)?;
            if r.updates.is_empty() {
                return Err(Error::Invalid("refs/update with zero updates".into()));
            }
            if r.updates.len() > limits.max_batch_objects {
                return Err(Error::Limit("too many ref updates in one request".into()));
            }
            let who = principal.map(|p| p.id.as_str()).unwrap_or("anonymous");
            let mut ops = Vec::with_capacity(r.updates.len());
            let mut names = Vec::new();
            for (i, u) in r.updates.iter().enumerate() {
                crate::repo::refs::check_ref_name_system(&u.name)
                    .map_err(|e| Error::InvalidRef(format!("update {i} ({}): {e}", u.name)))?;
                if is_internal_ref(&u.name) {
                    return Err(Error::InvalidRef(format!(
                        "update {i}: {} is a NewGit-internal namespace and cannot be updated remotely",
                        u.name
                    )));
                }
                let cas =
                    match &u.cas {
                        CasWire::Any => Cas::Any,
                        CasWire::Exactly { old } => Cas::Exactly(match old {
                            None => None,
                            Some(hex) => Some(ObjectId::from_hex(hex).map_err(|e| {
                                Error::InvalidRef(format!("update {i} cas.old: {e}"))
                            })?),
                        }),
                    };
                let new = match &u.new {
                    None => None,
                    Some(hex) => {
                        let oid = ObjectId::from_hex(hex)
                            .map_err(|e| Error::InvalidRef(format!("update {i} new: {e}")))?;
                        if !repo.objects.contains(&oid) {
                            return Err(Error::Protocol(format!(
                                "update {i}: target {oid} is not stored on the server \
                                 (upload objects before moving refs)"
                            )));
                        }
                        Some(oid)
                    }
                };
                let mut msg = format!("remote update by {who}");
                if let Some(extra) = &u.message {
                    msg.push_str(": ");
                    msg.push_str(extra);
                }
                ops.push(TxnOp::Ref {
                    name: u.name.clone(),
                    cas,
                    new,
                    log: RefLogEntry::system(msg),
                });
                names.push(u.name.clone());
            }
            // ONE transaction: any CAS failure ⇒ nothing moves (409).
            let report = txn::execute(repo.ng(), ops, limits)?;
            Ok(serde_json::to_value(UpdateData {
                updated: names,
                txn_id: report.txn_id,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/object") => {
            let r: ObjectReq = body_json(req)?;
            let oid =
                ObjectId::from_hex(&r.oid).map_err(|e| Error::Malformed(format!("oid: {e}")))?;
            let obj = repo.objects.get(&oid)?;
            let kind = obj.type_tag().name().to_string();
            let links: Vec<String> = verify::object_links(&obj)
                .into_iter()
                .map(|o| o.to_hex())
                .collect();
            let mut data = ObjectData {
                oid: oid.to_hex(),
                kind,
                links,
                data: None,
                data_b64: None,
                size: None,
            };
            match &obj {
                Object::Blob(bytes) => {
                    data.size = Some(bytes.len() as u64);
                    data.data_b64 = Some(crate::util::base64::encode(bytes));
                }
                other => {
                    // Object serializes as {"type":..,"data":..}; the UI wants
                    // the payload without the wrapper (kind is already a field).
                    let v = serde_json::to_value(other).map_err(|e| Error::Bug(e.to_string()))?;
                    data.data = Some(v.get("data").cloned().unwrap_or(v));
                }
            }
            Ok(serde_json::to_value(data).map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/diff") => {
            let r: DiffReq = body_json(req)?;
            check_wire_spec(&r.a)?;
            check_wire_spec(&r.b)?;
            let mut opts = crate::diff::DiffOpts::default();
            if r.no_renames {
                opts.detect_renames = false;
            }
            if let Some(c) = r.context {
                opts.context = c.min(100);
            }
            let a_root = crate::diff::resolve_tree(repo, &r.a)?;
            let b_root = crate::diff::resolve_tree(repo, &r.b)?;
            let td = crate::diff::diff_trees(repo, a_root, b_root, &opts)?;
            let mut unified = Vec::new();
            if r.content {
                const UNIFIED_CAP: usize = 100;
                for f in td
                    .files
                    .iter()
                    .filter(|f| f.kind == crate::diff::FileDiffKind::Modified && !f.binary)
                    .take(UNIFIED_CAP)
                {
                    let cd = crate::diff::diff_blob_content(repo, f.old_oid, f.new_oid, &opts)?;
                    let mut text = crate::diff::render::file_header(f);
                    text.push_str(&crate::diff::render::render_content(f, &cd, opts.context));
                    unified.push(UnifiedFile {
                        path: f.path.clone(),
                        unified: text,
                    });
                }
            }
            let diff_v = serde_json::to_value(&td).map_err(|e| Error::Bug(e.to_string()))?;
            Ok(serde_json::to_value(DiffData {
                a_root: a_root.to_hex(),
                b_root: b_root.to_hex(),
                diff: diff_v,
                unified,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("GET", "/v1/goals") | ("GET", "/v1/changes") | ("GET", "/v1/proposals") => {
            let tag = match req.path.as_str() {
                "/v1/goals" => crate::object::types::ObjectType::Goal,
                "/v1/changes" => crate::object::types::ObjectType::Change,
                _ => crate::object::types::ObjectType::Proposal,
            };
            let goal_filter = req
                .query_get("goal")
                .map(ObjectId::from_hex)
                .transpose()
                .map_err(|e| Error::Malformed(format!("goal filter: {e}")))?;
            let mut entities = Vec::new();
            for (oid, obj) in crate::ops::workflow::list_entities(repo, tag)? {
                if let (Some(g), Object::Change(c)) = (goal_filter, &obj) {
                    if c.goal != Some(g) {
                        continue;
                    }
                }
                let v = serde_json::to_value(&obj).map_err(|e| Error::Bug(e.to_string()))?;
                entities.push(EntityEntry {
                    oid: oid.to_hex(),
                    data: v,
                });
            }
            // bound the response (iteration-12 audit): cap at the batch limit
            let truncated = entities.len() > limits.max_batch_objects;
            if truncated {
                entities.truncate(limits.max_batch_objects);
            }
            Ok(serde_json::to_value(ListData {
                entities,
                truncated,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("GET", "/v1/audit") => {
            let limit: usize = req
                .query_get("limit")
                .map(|s| s.parse().unwrap_or(100))
                .unwrap_or(100)
                .min(10_000);
            let entries = AuditLog::new(repo.ng()).tail(limit)?;
            Ok(serde_json::to_value(AuditData { entries })
                .map_err(|e| Error::Bug(e.to_string()))?)
        }
        _ => Err(Error::Protocol(format!(
            "{} {} not supported",
            req.method, req.path
        ))),
    }
}

fn head_string(repo: &Repo) -> Result<String> {
    Ok(match repo.read_head()? {
        crate::repo::Head::Symbolic(name) => format!("ref: {name}"),
        crate::repo::Head::Detached(oid) => oid.to_hex(),
    })
}

fn body_json<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T> {
    if req.body.is_empty() {
        return Err(Error::Invalid("request body required".into()));
    }
    serde_json::from_slice(&req.body)
        .map_err(|e| Error::Malformed(format!("invalid JSON body: {e}")))
}

fn parse_oid_batch(hexes: &[String], cap: usize) -> Result<Vec<ObjectId>> {
    if hexes.len() > cap {
        return Err(Error::Limit(format!(
            "batch of {} exceeds cap {cap}",
            hexes.len()
        )));
    }
    hexes
        .iter()
        .map(|h| ObjectId::from_hex(h).map_err(|e| Error::Malformed(format!("oid {h:?}: {e}"))))
        .collect()
}

/// Convenience for tests/CLI: spawn + a writable "listening" announcement.
pub fn listening_line(addr: SocketAddr) -> String {
    format!("newgit serve: listening on http://{addr} (protocol v{PROTOCOL_VERSION})")
}

#[cfg(test)]
mod request_deadline_tests {
    use super::*;

    #[test]
    fn accepted_stream_blocks_until_its_read_timeout() {
        use std::io::Read;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let accept_deadline = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && Instant::now() < accept_deadline =>
                {
                    std::thread::yield_now();
                }
                Err(error) => panic!("cannot accept loopback connection: {error}"),
            }
        };
        let mut stream = set_accepted_stream_blocking(stream).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        let started = Instant::now();
        let error = stream.read(&mut [0_u8; 1]).unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "unexpected read result after idle wait: {error}"
        );
        assert!(
            elapsed >= Duration::from_millis(75),
            "accepted read returned after {elapsed:?}, before its 100 ms timeout"
        );
        drop(client);
    }

    fn test_config(repo_root: PathBuf, token_file: PathBuf) -> ServerConfig {
        ServerConfig {
            bind: "127.0.0.1:0".into(),
            repo_root,
            token_file,
            allow_anonymous_read: true,
            ..ServerConfig::default()
        }
    }

    /// Drive the production connection handler over loopback with a short
    /// accept-start budget; the sender can pause before and between bytes.
    fn request_with_budget(
        cfg: &ServerConfig,
        budget: Duration,
        initial: &[u8],
        drip: &[u8],
        pause_before_drip: Duration,
        per_byte_delay: Duration,
    ) -> (String, Duration) {
        use std::io::{Read, Write};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cfg = cfg.clone();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let deadline = Instant::now() + budget;
            handle_conn(stream, &cfg, deadline);
        });

        let mut writer = TcpStream::connect(addr).unwrap();
        writer.set_nodelay(true).unwrap();
        let mut reader = writer.try_clone().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let started = Instant::now();
        writer.write_all(initial).unwrap();

        let stop_sender = Arc::new(AtomicBool::new(false));
        let sender_stop = stop_sender.clone();
        let drip = drip.to_vec();
        let sender = std::thread::spawn(move || {
            if !pause_before_drip.is_zero() {
                std::thread::sleep(pause_before_drip);
            }
            for byte in drip {
                if sender_stop.load(Ordering::Relaxed) || writer.write_all(&[byte]).is_err() {
                    break;
                }
                if !per_byte_delay.is_zero() {
                    std::thread::sleep(per_byte_delay);
                }
            }
        });

        // Consume exactly one framed response instead of waiting for EOF while
        // the deliberately slow sender still owns its half of the test socket.
        let mut response = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let n = reader.read(&mut chunk).unwrap();
            assert_ne!(n, 0, "server closed before sending response headers");
            response.extend_from_slice(&chunk[..n]);
            if let Some(position) = response.windows(4).position(|w| w == b"\r\n\r\n") {
                break position + 4;
            }
        };
        stop_sender.store(true, Ordering::Relaxed);
        sender.join().unwrap();
        let header = std::str::from_utf8(&response[..header_end - 4]).unwrap();
        let content_length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .expect("response includes Content-Length");
        let response_end = header_end + content_length;
        while response.len() < response_end {
            let mut chunk = [0u8; 4096];
            let n = reader.read(&mut chunk).unwrap();
            assert_ne!(n, 0, "server closed before the response body completed");
            response.extend_from_slice(&chunk[..n]);
        }
        worker.join().unwrap();
        let response = String::from_utf8(response[..response_end].to_vec()).unwrap();
        // Native Windows CI observed WSAETIMEDOUT (10060), rather than EOF or
        // reset, on this extra-byte read after the complete response and worker
        // join. The joined handler has dropped its accepted-side streams; the
        // contract is checked portably by the response status/Connection: close,
        // deadline bounds, and worker completion below. Do not use peer EOF as
        // the Windows close oracle while the client write half remains open.
        #[cfg(not(windows))]
        {
            let mut extra = [0u8; 1];
            match reader.read(&mut extra) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) => {}
                result => panic!("server did not close the request socket: {result:?}"),
            }
        }
        (response, started.elapsed())
    }

    fn status(response: &str) -> u16 {
        response
            .lines()
            .next()
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .expect("HTTP response status")
    }

    #[test]
    fn absolute_request_deadline_covers_headers_and_body_and_allows_normal_requests() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        Repo::init(&repo_root).unwrap();
        let cfg = test_config(repo_root, dir.path().join("tokens.json"));
        let budget = Duration::from_millis(250);
        let drip = Duration::from_millis(20); // well below the production 30 s idle timeout

        let (header_response, header_elapsed) = request_with_budget(
            &cfg,
            budget,
            b"GET /v1/info HTTP/1.1",
            b"\r\nHost: x\r\nConnection: close\r\n\r\n",
            Duration::ZERO,
            drip,
        );
        assert_eq!(status(&header_response), 408, "{header_response}");
        assert!(header_response.contains("Connection: close\r\n"));
        assert!(
            header_elapsed >= budget.saturating_sub(Duration::from_millis(50)),
            "header request timed out before the accept-start deadline: {header_elapsed:?}"
        );
        assert!(
            header_elapsed < Duration::from_secs(2),
            "{header_elapsed:?}"
        );

        let body_budget = Duration::from_millis(400);
        let (body_response, body_elapsed) = request_with_budget(
            &cfg,
            body_budget,
            b"POST /v1/have HTTP/1.1\r\nHost: x\r\nContent-Length: 20\r\nConnection: close\r\n\r\n",
            b"01234567890123456789",
            Duration::from_millis(330),
            drip,
        );
        assert_eq!(status(&body_response), 408, "{body_response}");
        assert!(body_response.contains("Connection: close\r\n"));
        assert!(
            body_elapsed >= body_budget.saturating_sub(Duration::from_millis(50)),
            "body request timed out before the accept-start deadline: {body_elapsed:?}"
        );
        assert!(body_elapsed < Duration::from_secs(1), "{body_elapsed:?}");

        let (normal_response, _) = request_with_budget(
            &cfg,
            Duration::from_secs(2),
            b"GET /v1/info HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            b"",
            Duration::ZERO,
            Duration::ZERO,
        );
        assert_eq!(status(&normal_response), 200, "{normal_response}");
        assert!(normal_response.contains("\"product\":\"newgit\""));
    }
}
