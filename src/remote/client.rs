//! NewGit remote client — `remote add/list/remove`, `push`, `pull`.
//!
//! Plain HTTP/1.1 client on std TcpStream (no deps). v1 speaks
//! Content-Length JSON only, one request per connection. Push and pull are
//! negotiated (only missing objects travel), dependency-ordered (the
//! receiver's link-closure check can always pass), and crash-safe: objects
//! are idempotent, refs move in ONE transaction on the receiving side.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::object::envelope;
use crate::object::types::Object;
use crate::object::ObjectId;
use crate::ops::verify;
use crate::remote::proto::*;
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::util::{base64, fsx};

pub const REMOTES_FILE: &str = "remotes.json";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(300);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Remote configuration (.newgit/remotes.json)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    /// `http://host[:port]` — v1 is plain HTTP (TLS via reverse proxy).
    pub url: String,
    /// Raw bearer token. Stored 0600; never printed by `remote list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemotesFile {
    pub remotes: Vec<Remote>,
}

pub fn remotes_path(repo: &Repo) -> std::path::PathBuf {
    repo.ng().join(REMOTES_FILE)
}

pub fn load_remotes(repo: &Repo) -> Result<RemotesFile> {
    load_remotes_from(&remotes_path(repo))
}

pub fn load_remotes_from(path: &Path) -> Result<RemotesFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RemotesFile::default()),
        Err(e) => Err(Error::Io {
            path: Some(path.to_path_buf()),
            source: e,
        }),
    }
}

pub fn save_remotes(repo: &Repo, rf: &RemotesFile) -> Result<()> {
    let path = remotes_path(repo);
    let json = serde_json::to_vec_pretty(rf).map_err(|e| Error::Bug(e.to_string()))?;
    fsx::atomic_write(&path, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn add_remote(repo: &Repo, name: &str, url: &str, token: Option<&str>) -> Result<()> {
    validate_url(url)?;
    if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '/') {
        return Err(Error::Invalid(format!("remote name {name:?} invalid")));
    }
    let mut rf = load_remotes(repo)?;
    if rf.remotes.iter().any(|r| r.name == name) {
        return Err(Error::Invalid(format!("remote {name:?} already exists")));
    }
    rf.remotes.push(Remote {
        name: name.to_string(),
        url: url.to_string(),
        token: token.map(|t| t.to_string()),
    });
    save_remotes(repo, &rf)
}

pub fn remove_remote(repo: &Repo, name: &str) -> Result<bool> {
    let mut rf = load_remotes(repo)?;
    let before = rf.remotes.len();
    rf.remotes.retain(|r| r.name != name);
    if rf.remotes.len() == before {
        return Ok(false);
    }
    save_remotes(repo, &rf)?;
    Ok(true)
}

pub fn get_remote(repo: &Repo, name: &str) -> Result<Remote> {
    load_remotes(repo)?
        .remotes
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            Error::Config(format!(
                "no remote named {name:?} (see `newgit remote list`)"
            ))
        })
}

/// Accept only `http://host[:port]` (plain-HTTP v1); everything else gets an
/// actionable error (especially `https://` — a common misconception).
pub fn validate_url(url: &str) -> Result<(String, u16)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        Error::Invalid(format!(
            "remote url {url:?}: v1 speaks plain HTTP only — use http://host[:port] \
             (put TLS in front via a reverse proxy; see docs/PROTOCOL.md)"
        ))
    })?;
    if rest.is_empty() {
        return Err(Error::Invalid("remote url has empty host".into()));
    }
    // No paths in v1 (server serves the repo it was started in).
    let hostport = rest.split('/').next().unwrap_or(rest);
    if hostport != rest {
        return Err(Error::Invalid(format!(
            "remote url {url:?}: paths are not supported in protocol v1"
        )));
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| Error::Invalid(format!("bad port in url {url:?}")))?;
            (h.to_string(), port)
        }
        None => (hostport.to_string(), 80),
    };
    if host.is_empty() {
        return Err(Error::Invalid(format!("empty host in url {url:?}")));
    }
    Ok((host, port))
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Client {
    host: String,
    port: u16,
    token: Option<String>,
}

impl Client {
    pub fn from_remote(remote: &Remote) -> Result<Client> {
        let (host, port) = validate_url(&remote.url)?;
        Ok(Client {
            host,
            port,
            token: remote.token.clone(),
        })
    }

    fn connect(&self) -> Result<TcpStream> {
        let addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| Error::Protocol(format!("resolve {}:{}: {e}", self.host, self.port)))?;
        let mut last = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(READ_TIMEOUT));
                    let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(Error::Protocol(format!(
            "cannot connect to {}:{}: {}",
            self.host,
            self.port,
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// One request/response exchange. Returns the `data` payload on 2xx or
    /// maps the server envelope to a typed Error (auth/race/limit/...).
    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut stream = self.connect()?;
        let body_bytes = match body {
            Some(v) => serde_json::to_vec(v).map_err(|e| Error::Bug(e.to_string()))?,
            None => Vec::new(),
        };
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\n{HDR_PROTOCOL}: {PROTOCOL_VERSION}\r\nConnection: close\r\n",
            self.host, self.port
        );
        if let Some(tok) = &self.token {
            head.push_str(&format!("Authorization: Bearer {tok}\r\n"));
        }
        if body.is_some() {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body_bytes.len()
            ));
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .and_then(|_| stream.write_all(&body_bytes))
            .and_then(|_| stream.flush())
            .map_err(|e| Error::Protocol(format!("request write: {e}")))?;

        let (status, resp_body) = read_response(&mut stream)?;
        let envelope: Value = if resp_body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&resp_body).map_err(|e| {
                Error::Protocol(format!("server sent non-JSON body (status {status}): {e}"))
            })?
        };
        if (200..300).contains(&status) {
            if envelope.get("ok") == Some(&json!(true)) {
                return Ok(envelope.get("data").cloned().unwrap_or(Value::Null));
            }
            return Err(Error::Protocol(format!(
                "server returned status {status} without ok:true envelope"
            )));
        }
        // Error path: prefer the server's own category/message.
        let category = envelope
            .pointer("/error/category")
            .and_then(|v| v.as_str())
            .unwrap_or("protocol")
            .to_string();
        let message = envelope
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .map(|m| m.to_string())
            .unwrap_or_else(|| format!("server error (HTTP {status})"));
        Err(match (status, category.as_str()) {
            (401 | 403, _) => Error::Auth(format!("{message} (HTTP {status})")),
            (409, "cas_failed") => Error::CasFailed(format!("{message} (HTTP 409)")),
            (409, "lock_busy") => Error::LockBusy(format!("{message} (HTTP 409)")),
            (409, _) => Error::Conflict(format!("{message} (HTTP 409)")),
            (413, _) | (_, "limit") => Error::Limit(format!("{message} (HTTP {status})")),
            (404, _) => Error::RefNotFound(format!("{message} (HTTP 404)")),
            (_, "malformed") => Error::Malformed(format!("{message} (HTTP {status})")),
            (_, "auth") => Error::Auth(format!("{message} (HTTP {status})")),
            _ => Error::Protocol(format!("{message} (HTTP {status})")),
        })
    }
}

/// Read a response: status line, headers, Content-Length body. Total and
/// capped — a hostile "server" cannot make the client hang or explode.
fn read_response<R: Read>(r: &mut R) -> Result<(u16, Vec<u8>)> {
    const MAX_HEAD: usize = 64 * 1024;
    const MAX_BODY: u64 = 2 * 1024 * 1024 * 1024; // negotiated batches are client-capped anyway
    let mut br = BufReader::new(r);
    let mut status_line = String::new();
    if br
        .read_line(&mut status_line)
        .map_err(|e| Error::Protocol(format!("status line: {e}")))?
        == 0
    {
        return Err(Error::Protocol(
            "server closed connection without a response".into(),
        ));
    }
    if status_line.len() > MAX_HEAD {
        return Err(Error::Protocol("status line too long".into()));
    }
    let mut parts = status_line.trim_end().splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(Error::Protocol(format!("bad response version {version:?}")));
    }
    let status: u16 = code
        .parse()
        .map_err(|_| Error::Protocol(format!("bad status code {code:?}")))?;
    let mut content_length: Option<u64> = None;
    let mut total = 0usize;
    loop {
        let mut hl = String::new();
        let n = br
            .read_line(&mut hl)
            .map_err(|e| Error::Protocol(format!("header: {e}")))?;
        if n == 0 {
            return Err(Error::Protocol(
                "connection closed inside response headers".into(),
            ));
        }
        total += n;
        if total > MAX_HEAD {
            return Err(Error::Protocol("response headers too large".into()));
        }
        let hl = hl.trim_end();
        if hl.is_empty() {
            break;
        }
        if let Some((k, v)) = hl.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok().filter(|l: &u64| *l <= MAX_BODY);
            }
        }
    }
    let len = content_length.ok_or_else(|| {
        Error::Protocol("response without Content-Length (chunked not supported in v1)".into())
    })?;
    let mut body = vec![0u8; len as usize];
    br.read_exact(&mut body)
        .map_err(|e| Error::Protocol(format!("response body: {e}")))?;
    Ok((status, body))
}

// ---------------------------------------------------------------------------
// Protocol-level helpers
// ---------------------------------------------------------------------------

fn server_info(c: &Client) -> Result<InfoData> {
    let v = c.call("GET", "/v1/info", None)?;
    let info: InfoData =
        serde_json::from_value(v).map_err(|e| Error::Protocol(format!("bad /v1/info: {e}")))?;
    if info.protocol != PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "server speaks protocol v{} but this client speaks v{PROTOCOL_VERSION} — upgrade one of them",
            info.protocol
        )));
    }
    Ok(info)
}

fn server_refs(c: &Client) -> Result<(String, Vec<RefEntry>)> {
    let v = c.call("GET", "/v1/refs", None)?;
    let data: RefsData =
        serde_json::from_value(v).map_err(|e| Error::Protocol(format!("bad /v1/refs: {e}")))?;
    Ok((data.head, data.refs))
}

/// Ask the server which of `oids` it stores. Batched to `max_batch_objects`.
fn server_have(c: &Client, oids: &[ObjectId], batch: usize) -> Result<HashSet<ObjectId>> {
    let mut known = HashSet::new();
    for chunk in oids.chunks(batch.max(1)) {
        let req = HaveReq {
            oids: chunk.iter().map(|o| o.to_hex()).collect(),
        };
        let v = c.call(
            "POST",
            "/v1/have",
            Some(&serde_json::to_value(&req).map_err(|e| Error::Bug(e.to_string()))?),
        )?;
        let data: HaveData =
            serde_json::from_value(v).map_err(|e| Error::Protocol(format!("bad /v1/have: {e}")))?;
        for h in &data.have {
            known.insert(ObjectId::from_hex(h)?);
        }
    }
    Ok(known)
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

/// Push `ref_names` (NewGit ref names) to the remote. `force` downgrades
/// the ref CAS from exactly(old-observed) to any.
pub fn push(repo: &Repo, remote: &Remote, ref_names: &[String], force: bool) -> Result<PushReport> {
    let c = Client::from_remote(remote)?;
    let info = server_info(&c)?;
    let batch = info.limits.max_batch_objects.max(1);
    let (_shead, srefs) = server_refs(&c)?;
    let server_tips: std::collections::BTreeMap<String, ObjectId> = srefs
        .iter()
        .map(|r| Ok((r.name.clone(), ObjectId::from_hex(&r.oid)?)))
        .collect::<Result<_>>()?;

    // Resolve local tips for the requested refs.
    let mut wanted: Vec<(String, ObjectId)> = Vec::new();
    for name in ref_names {
        let oid = repo.refs.read(name).map_err(|_| {
            Error::RefNotFound(format!("local ref {name} does not exist — nothing to push"))
        })?;
        wanted.push((name.clone(), oid));
    }

    // Non-fast-forward guard (git semantics): without --force, the observed
    // server tip must be an ANCESTOR of the local tip. A stale clone (or a
    // diverged server) gets an actionable Conflict instead of a clobber.
    // The wire-level CAS still protects the observe→update window itself.
    if !force {
        for (name, tip) in &wanted {
            if let Some(st) = server_tips.get(name) {
                if st == tip {
                    continue;
                }
                if !repo.objects.contains(st) {
                    return Err(Error::Conflict(format!(
                        "non-fast-forward: server ref {name} is at {st}, which this repo                          does not have — `newgit pull` first, or push --force"
                    )));
                }
                let closure = crate::remote::negotiate::closure(repo, &[*tip]);
                if !closure.contains(st) {
                    return Err(Error::Conflict(format!(
                        "non-fast-forward: server ref {name} at {st} is not an ancestor of                          local tip {tip} — pull and integrate first, or push --force"
                    )));
                }
            }
        }
    }

    // Negotiation: BFS from local tips; probe the server in batches; stop
    // descending at oids the server already holds (have-invariant: the
    // server's store is link-closed, so holding X means holding closure(X)).
    let mut known_server: HashSet<ObjectId> = HashSet::new();
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut frontier: Vec<ObjectId> = wanted.iter().map(|(_, o)| *o).collect();
    let mut probes = 0usize;
    loop {
        frontier.sort();
        frontier.dedup();
        frontier.retain(|o| !visited.contains(o));
        if frontier.is_empty() {
            break;
        }
        probes += 1;
        let have = server_have(&c, &frontier, batch)?;
        visited.extend(frontier.iter().copied());
        known_server.extend(have.iter().copied());
        let mut next = Vec::new();
        for oid in &frontier {
            if have.contains(oid) {
                continue; // server holds the full closure — do not descend
            }
            let obj = repo.objects.get(oid)?;
            next.extend(verify::object_links(&obj));
        }
        frontier = next;
    }

    // Send set: full closure of wanted tips, pruned at server-held oids,
    // dependency-first.
    let send = crate::remote::negotiate::post_order(
        repo,
        &wanted.iter().map(|(_, o)| *o).collect::<Vec<_>>(),
        &known_server,
    )?;

    // Upload in dependency-ordered batches (raw self-verifying envelopes).
    let mut bytes_sent = 0u64;
    for chunk in send.chunks(batch) {
        let mut objects = Vec::with_capacity(chunk.len());
        for oid in chunk {
            let raw = std::fs::read(repo.objects.path_for(oid)).map_err(|e| Error::Io {
                path: Some(repo.objects.path_for(oid)),
                source: e,
            })?;
            bytes_sent += raw.len() as u64;
            objects.push(ObjectWire {
                data_b64: base64::encode(&raw),
            });
        }
        let req = ObjectsPutReq { objects };
        c.call(
            "POST",
            "/v1/objects/put",
            Some(&serde_json::to_value(&req).map_err(|e| Error::Bug(e.to_string()))?),
        )?;
    }

    // Move refs in ONE server transaction, CAS against the tips observed
    // at the start (any ⇒ --force).
    let updates: Vec<RefUpdateWire> = wanted
        .iter()
        .map(|(name, oid)| RefUpdateWire {
            name: name.clone(),
            cas: if force {
                CasWire::Any
            } else {
                CasWire::Exactly {
                    old: server_tips.get(name).map(|o| o.to_hex()),
                }
            },
            new: Some(oid.to_hex()),
            message: Some(format!("push from {}", repo_name(repo))),
        })
        .collect();
    let req = RefsUpdateReq { updates };
    c.call(
        "POST",
        "/v1/refs/update",
        Some(&serde_json::to_value(&req).map_err(|e| Error::Bug(e.to_string()))?),
    )?;

    Ok(PushReport {
        remote: remote.name.clone(),
        url: remote.url.clone(),
        refs_pushed: wanted.iter().map(|(n, _)| n.clone()).collect(),
        objects_sent: send.len(),
        bytes_sent,
        had_probe_requests: probes,
    })
}

fn repo_name(repo: &Repo) -> String {
    repo.root()
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into())
}

// ---------------------------------------------------------------------------
// pull
// ---------------------------------------------------------------------------

/// Pull `ref_filter` (None ⇒ every non-internal server ref) into the local
/// repo: fetch missing objects, then move local refs in ONE transaction
/// (CAS against the local tips observed at start — a concurrent local
/// writer wins and the pull fails cleanly instead of clobbering).
pub fn pull(repo: &Repo, remote: &Remote, ref_filter: Option<&[String]>) -> Result<PullReport> {
    let c = Client::from_remote(remote)?;
    let info = server_info(&c)?;
    let batch = info.limits.max_batch_objects.max(1);
    let (_shead, srefs) = server_refs(&c)?;
    let selected: Vec<&RefEntry> = srefs
        .iter()
        .filter(|r| !is_internal_ref(&r.name))
        .filter(|r| match ref_filter {
            None => true,
            Some(names) => names.iter().any(|n| n == &r.name),
        })
        .collect();
    if selected.is_empty() {
        return Ok(PullReport {
            remote: remote.name.clone(),
            url: remote.url.clone(),
            ..Default::default()
        });
    }

    // Haves: every local ref tip + HEAD (each is a closure root locally).
    let local_tips: Vec<ObjectId> = repo
        .refs
        .list(None)?
        .into_iter()
        .map(|(_, o)| o)
        .chain(repo.resolve_head()?)
        .filter(|o| repo.objects.contains(o))
        .collect();
    let want: Vec<ObjectId> = selected
        .iter()
        .map(|r| ObjectId::from_hex(&r.oid))
        .collect::<Result<_>>()?;

    let req = NegotiateReq {
        have: local_tips.iter().map(|o| o.to_hex()).collect(),
        want: want.iter().map(|o| o.to_hex()).collect(),
    };
    let v = c.call(
        "POST",
        "/v1/negotiate",
        Some(&serde_json::to_value(&req).map_err(|e| Error::Bug(e.to_string()))?),
    )?;
    let data: NegotiateData = serde_json::from_value(v)
        .map_err(|e| Error::Protocol(format!("bad /v1/negotiate: {e}")))?;
    let send: Vec<ObjectId> = data
        .send
        .iter()
        .map(|h| ObjectId::from_hex(h))
        .collect::<Result<_>>()?;

    // Fetch in batches; write each object through the SAME validation the
    // server applies (envelope verify + link-closure), dependency order
    // guaranteed by the server's post-order emission.
    let mut bytes_received = 0u64;
    let mut received = 0usize;
    for chunk in send.chunks(batch) {
        let req = ObjectsGetReq {
            ids: chunk.iter().map(|o| o.to_hex()).collect(),
        };
        let v = c.call(
            "POST",
            "/v1/objects/get",
            Some(&serde_json::to_value(&req).map_err(|e| Error::Bug(e.to_string()))?),
        )?;
        let data: ObjectsData = serde_json::from_value(v)
            .map_err(|e| Error::Protocol(format!("bad /v1/objects/get: {e}")))?;
        if data.objects.len() != chunk.len() {
            return Err(Error::Protocol(
                "server returned a different object count than requested".into(),
            ));
        }
        for (oid, wire) in chunk.iter().zip(data.objects.iter()) {
            let bytes = base64::decode(&wire.data_b64)
                .map_err(|e| Error::Protocol(format!("bad base64 from server: {e}")))?;
            bytes_received += bytes.len() as u64;
            let (canonical, got) = envelope::verify(&bytes, repo.limits().max_object_bytes)
                .map_err(|e| {
                    Error::Protocol(format!("server sent an invalid envelope for {oid}: {e}"))
                })?;
            if got != *oid {
                return Err(Error::Protocol(format!(
                    "server sent object {got} where {oid} was requested"
                )));
            }
            let decoded = Object::from_canonical(&canonical)
                .map_err(|e| Error::Protocol(format!("object {oid}: {e}")))?;
            for link in verify::object_links(&decoded) {
                if !repo.objects.contains(&link) {
                    return Err(Error::Protocol(format!(
                        "object {oid} from server has missing dependency {link} \
                         (server violated dependency ordering)"
                    )));
                }
            }
            repo.objects.put_canonical(&canonical)?;
            received += 1;
        }
    }

    // Move local refs in ONE transaction. CAS against observed local tips.
    let mut ops = Vec::new();
    let mut updated = Vec::new();
    let mut up_to_date = Vec::new();
    for r in &selected {
        let remote_oid = ObjectId::from_hex(&r.oid)?;
        let local = repo.refs.read_opt(&r.name)?;
        if local == Some(remote_oid) {
            up_to_date.push(r.name.clone());
            continue;
        }
        ops.push(TxnOp::Ref {
            name: r.name.clone(),
            cas: Cas::Exactly(local),
            new: Some(remote_oid),
            log: RefLogEntry::system(format!("pull from remote {} ({})", remote.name, remote.url)),
        });
        updated.push(r.name.clone());
    }
    let txn_id = if ops.is_empty() {
        None
    } else {
        Some(txn::execute(repo.ng(), ops, repo.limits())?.txn_id)
    };

    Ok(PullReport {
        remote: remote.name.clone(),
        url: remote.url.clone(),
        refs_updated: updated,
        refs_up_to_date: up_to_date,
        objects_received: received,
        bytes_received,
        txn_id,
    })
}

/// Default ref selection for `push` with no explicit refs: the ref HEAD
/// points at (symbolic), else an actionable error.
pub fn default_push_refs(repo: &Repo) -> Result<Vec<String>> {
    match repo.read_head()? {
        crate::repo::Head::Symbolic(name) => Ok(vec![name]),
        crate::repo::Head::Detached(_) => Err(Error::Invalid(
            "HEAD is detached — push explicit refs: `newgit push <remote> <ref>...` or --all"
                .into(),
        )),
    }
}

/// All pushable local refs (internal namespaces excluded).
pub fn all_push_refs(repo: &Repo) -> Result<Vec<String>> {
    Ok(repo
        .refs
        .list(None)?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|n| !is_internal_ref(n))
        .collect())
}
