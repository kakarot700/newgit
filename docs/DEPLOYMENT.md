# Deploying NewGit

Zero-rupee deployment: one static binary, one directory, no services, no
cloud. This guide covers a single-node production setup; everything runs on
a $0 box you already own (or a free-tier VM).

## 1. Install

From a dist tarball (`scripts/dist.sh` output):

```bash
tar xzf newgit-0.1.0-x86_64-unknown-linux-gnu.tar.gz
cd newgit-0.1.0-x86_64-unknown-linux-gnu
sha256sum -c SHA256SUMS.txt          # verify BEFORE installing
sudo install -m 0755 bin/newgit /usr/local/bin/newgit
newgit --version
```

Or build from source (toolchain pinned by `rust-toolchain.toml`, 1.99.0):

```bash
cargo build --release && sudo install -m 0755 target/release/newgit /usr/local/bin/
```

Reproducibility: release builds are bit-identical across clean target dirs on
the same host+toolchain (verified 2026-10-04, sha256 `abcd51c8…` twice; see
docs/BENCHMARKS.md environment). Cross-host builds need the same rustc
version and source path for byte-equality — checksums, not blind trust, are
the verification story.

## 2. Server

### 2.1 Bootstrap

```bash
newgit init /srv/newgit/mainrepo        # or: newgit import-git <existing-git-repo>
cd /srv/newgit/mainrepo
newgit token add alice --role admin     # raw token printed ONCE — store it now
newgit token add ci-bot --role write
newgit token add reader --role read
# tokens live hashed (SHA-256) in .newgit/tokens.json (mode 0600)
```

### 2.2 systemd unit

```ini
# /etc/systemd/system/newgit.service
[Unit]
Description=NewGit remote server (protocol v1)
After=network.target

[Service]
Type=simple
User=newgit
Group=newgit
ExecStart=/usr/local/bin/newgit --repo /srv/newgit/mainrepo serve \
    --bind 127.0.0.1:8765 --token-file /srv/newgit/mainrepo/.newgit/tokens.json
Restart=on-failure
RestartSec=2
# hardening — the server needs the repo dir and nothing else
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/srv/newgit/mainrepo
PrivateTmp=true
LimitNOFILE=4096

[Install]
WantedBy=multi-user.target
```

Notes:
- Bind **127.0.0.1** and terminate TLS at a reverse proxy (§3). NewGit v1
  speaks plain HTTP by design (D-017); do not expose the port directly.
- Crash safety needs no special shutdown handling: transactions journal,
  objects write atomically; a hard kill recovers on next open (tested by
  the chaos/fault-injection suites). `Restart=on-failure` is sufficient.
- One server = one repository (KL #29). Run one unit per repo.

### 2.3 TLS reverse proxy (nginx)

```nginx
server {
    listen 443 ssl;
    server_name git.example.org;
    ssl_certificate     /etc/letsencrypt/live/git.example.org/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/git.example.org/privkey.pem;
    client_max_body_size 64m;              # matches newgit max_request_bytes
    location / {
        proxy_pass http://127.0.0.1:8765;
        proxy_http_version 1.1;            # NewGit speaks HTTP/1.1, no chunked
        proxy_set_header Connection close; # server closes per request anyway
        proxy_read_timeout 60s;
        proxy_pass_header X-NewGit-Protocol;
    }
}
```

Caddy equivalent (automatic HTTPS):

```
git.example.org {
    request_body { max_size 64MB }
    reverse_proxy 127.0.0.1:8765
}
```

Let's Encrypt via certbot/caddy is $0. The protocol is proxy-friendly:
Content-Length framing only, one request per connection, no websockets.

**Who the TLS proxy serves:** browsers (Web UI) and HTTP-API agents (curl,
MCP-over-HTTP wrappers, CI) — anything that speaks HTTPS itself. The
`newgit` CLI client is plain-HTTP-only in v1 (`validate_url` REJECTS
`https://` URLs with an actionable error — no half-TLS, no faking): for
`push`/`pull` across untrusted networks use a tunnel:

```bash
ssh -N -L 8765:127.0.0.1:8765 git.example.org &     # $0 with any ssh access
newgit remote add origin http://127.0.0.1:8765 --token <raw-token>
```

Client-side TLS is a v2 protocol candidate (PROTOCOL.md versioning).

### 2.4 Web UI

```bash
newgit --repo /srv/newgit/mainrepo ui --bind 127.0.0.1:8765
```

adds the read-only explorer at `/` on the same server (same tokens, same
audit log). No extra ports, no extra services, no auth bypass: the HTML
shell is data-free, all data stays behind role-gated `/v1/*` endpoints.

## 3. Clients & agents

```bash
# v1 remotes are http:// URLs — TLS via proxy (UI/API) or tunnel (CLI, §2.3)
newgit remote add origin http://127.0.0.1:8765 --token <raw-token>
newgit push origin --all
newgit pull origin
```

### 3.1 Standard Git clients (read-only)

The same single-repository server exposes Git smart HTTP at its root. If
anonymous reads are enabled, ordinary clients can use:

```bash
git clone http://127.0.0.1:8765/ clone
git -C clone ls-remote origin
git -C clone fetch origin
git -C clone pull --ff-only
```

With the default authenticated-read policy, use a reader token as an HTTP
`Authorization: Bearer` header. For an interactive one-off session, avoid
putting the raw token in shell history or the remote URL:

```bash
read -rsp 'NewGit reader token: ' NEWGIT_TOKEN; echo
git_with_newgit_auth() {
  GIT_CONFIG_COUNT=1 \
  GIT_CONFIG_KEY_0=http.extraHeader \
  GIT_CONFIG_VALUE_0="Authorization: Bearer ${NEWGIT_TOKEN}" \
    git "$@"
}
git_with_newgit_auth clone http://127.0.0.1:8765/ clone
git_with_newgit_auth -C clone fetch origin
git_with_newgit_auth -C clone pull --ff-only
unset NEWGIT_TOKEN
```

Use the root URL (`/`); this server instance has no multi-repository or URL
path routing. The built-in listener is plain HTTP; expose Git clients over
HTTPS only through a trusted TLS-terminating reverse proxy (see §2.3).
Git smart HTTP supports upload-pack plus a narrow receive-pack path: one or
more write-token-authenticated branch creates or fast-forward updates per
request, committed together in NewGit. Tags, deletes, and non-fast-forward
pushes are refused; a partially accepted request changes no canonical refs.
Git's separate `--atomic` push capability is not advertised or supported. The
server requires the system `git` executable.
The existing `--max-body` setting caps both inbound HTTP request bodies and
buffered Git pack/advertisement responses (default 64 MiB); increase it for
larger packs. Each Git request has a 120-second processing deadline, but there
is no separate temporary-disk or peak-memory quota; large histories or several
simultaneous Git requests can still create significant resource pressure.

Agents: see docs/AGENT_GUIDE.md (HTTP API with curl recipes, `newgit mcp`
for MCP clients). MCP servers are spawned per-agent, per-repo, under that
agent's OS user — stdio only, no ports.

## 4. Operations

| Task | Command / policy |
|---|---|
| Health | `GET /healthz` (anonymous) or `newgit verify` locally |
| Integrity check | `newgit verify --deep` (read-only fsck, stable issue codes, exit 3 on findings) |
| Garbage collection | `newgit gc` (strictly non-destructive; 24h grace on unreachable objects; `gc --dry-run` first if cautious) |
| Audit | `newgit audit -n 200` or `GET /v1/audit` (admin) — every request incl. failures |
| Backup | the repo is ONE directory: `rsync -a /srv/newgit/mainrepo/ backup-host:…` while idle, OR push all refs to a second `newgit serve` (incremental, negotiated). Objects are content-addressed ⇒ backups are self-verifying (`newgit verify` on the copy). |
| Restore | copy back, `newgit verify --deep`, done. Journals recover automatically on first open. |
| Limits | `.newgit/config`: blob/object/stored size caps, tree entries, walk depth, lock waits, batch counts; serve flags: `--max-body`, `--max-threads` |
| Upgrade | install new binary, restart unit. On-disk format is versioned (`format_version = 1`); migration notes live in docs/STORAGE_FORMAT.md |
| Logs | structured JSON events on stderr with `--debug` (or `NEWGIT_LOG=1`); audit log is the security record |

## 5. Scaling notes (honest)

- Thread-per-connection with a bounded pool (default 32; 429 beyond). Fine
  for team/agent-scale (tens of concurrent clients); it is not a CDN.
- Every connection reopens the repo (recovery scan) — cheap for normal
  repos, measurable for pathological journal backlogs (KL #30).
- No horizontal scaling / multi-primary in v1: one writer process per repo
  directory (locks are local). Replicate by pushing to a second server.
