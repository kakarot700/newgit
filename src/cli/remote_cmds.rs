//! CLI commands for the remote layer: `serve`, `remote`, `push`, `pull`,
//! `token`. Wire protocol: docs/PROTOCOL.md (v1).

use serde_json::json;

use crate::cli::args::{Args, COMMON_ALIASES};
use crate::cli::{open_repo, Ctx, Output};
use crate::error::{Error, Result};
use crate::obs;
use crate::remote::auth::{self, Role};
use crate::remote::client;
use crate::remote::server::{self, ServerConfig, DEFAULT_BIND};

pub(crate) fn dispatch(ctx: &Ctx, cmd: &str, tail: &[String]) -> Result<Output> {
    match cmd {
        "serve" => cmd_serve(ctx, tail),
        "remote" => cmd_remote(ctx, tail),
        "push" => cmd_push(ctx, tail),
        "pull" | "fetch" => cmd_pull(ctx, tail),
        "token" => cmd_token(ctx, tail),
        "audit" => cmd_audit(ctx, tail),
        _ => Err(Error::Bug(format!("remote_cmds: unknown {cmd}"))),
    }
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

fn cmd_serve(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(
        tail,
        &["bind", "token-file", "max-body", "max-threads"],
        COMMON_ALIASES,
    )?;
    a.reject_unknown(&[
        "json",
        "debug",
        "repo",
        "bind",
        "token-file",
        "max-body",
        "max-threads",
        "allow-anonymous-read",
    ])?;
    let repo = open_repo(ctx)?;
    let bind = a.opt("bind").unwrap_or(DEFAULT_BIND).to_string();
    let token_file = a
        .opt("token-file")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.ng().join("tokens.json"));
    let max_body = match a.opt("max-body") {
        Some(s) => s
            .parse::<u64>()
            .map_err(|_| Error::Invalid(format!("--max-body must be a byte count, got {s:?}")))?,
        None => 64 * 1024 * 1024,
    };
    let max_threads = match a.opt("max-threads") {
        Some(s) => s
            .parse::<usize>()
            .map_err(|_| Error::Invalid(format!("--max-threads must be a number, got {s:?}")))?
            .clamp(1, 1024),
        None => 32,
    };
    let cfg = ServerConfig {
        bind,
        repo_root: repo.root().to_path_buf(),
        token_file,
        allow_anonymous_read: a.flag("allow-anonymous-read"),
        max_body,
        max_threads,
    };
    // Fail fast on an unusable token file BEFORE binding.
    auth::load(&cfg.token_file)?;
    let handle = server::spawn(cfg)?;
    let line = server::listening_line(handle.addr());
    obs::event(
        "serve_listening",
        &[("addr", json!(handle.addr().to_string()))],
    );
    if ctx.json {
        println!(
            "{}",
            json!({
                "ok": true,
                "data": {
                    "listening": handle.addr().to_string(),
                    "protocol": crate::remote::proto::PROTOCOL_VERSION,
                }
            })
        );
    } else {
        println!("{line}");
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // Block until the process is signalled (SIGINT/SIGTERM kill it; the
    // repo is crash-safe by construction — journals recover on next open).
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

// ---------------------------------------------------------------------------
// remote add/list/remove
// ---------------------------------------------------------------------------

fn cmd_remote(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["token"], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "token"])?;
    let repo = open_repo(ctx)?;
    let pos = a.positional();
    match pos.first().map(String::as_str) {
        Some("add") => {
            let name = get(pos, 1, "remote name")?;
            let url = get(pos, 2, "remote url")?;
            client::add_remote(&repo, name, url, a.opt("token"))?;
            finish(
                ctx,
                json!({ "added": name, "url": url, "token": a.opt("token").is_some() }),
                format!("remote {name} → {url} added"),
            )
        }
        Some("list") | Some("ls") | None => {
            let rf = client::load_remotes(&repo)?;
            if ctx.json {
                let entries: Vec<_> = rf
                    .remotes
                    .iter()
                    .map(|r| {
                        json!({
                            "name": r.name,
                            "url": r.url,
                            "token": r.token.is_some(),
                        })
                    })
                    .collect();
                return Ok(Output::Json(json!(entries)));
            }
            let mut t = String::new();
            if rf.remotes.is_empty() {
                t.push_str("no remotes configured (see `newgit remote add`)\n");
            }
            for r in &rf.remotes {
                t.push_str(&format!(
                    "{}\t{}\ttoken={}\n",
                    r.name,
                    r.url,
                    if r.token.is_some() { "set" } else { "none" }
                ));
            }
            Ok(Output::Text(t.trim_end().to_string()))
        }
        Some("remove") | Some("rm") => {
            let name = get(pos, 1, "remote name")?;
            let removed = client::remove_remote(&repo, name)?;
            if !removed {
                return Err(Error::Config(format!("no remote named {name:?}")));
            }
            finish(
                ctx,
                json!({ "removed": name }),
                format!("remote {name} removed"),
            )
        }
        Some(other) => Err(Error::Invalid(format!(
            "unknown remote subcommand {other:?}; try add|list|remove"
        ))),
    }
}

// ---------------------------------------------------------------------------
// push / pull
// ---------------------------------------------------------------------------

fn cmd_push(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "all", "force"])?;
    let repo = open_repo(ctx)?;
    let pos = a.positional();
    let name = get(pos, 0, "remote name")?;
    let remote = client::get_remote(&repo, name)?;
    let refs: Vec<String> = if a.flag("all") {
        if pos.len() > 1 {
            return Err(Error::Invalid("--all takes no explicit refs".into()));
        }
        client::all_push_refs(&repo)?
    } else if pos.len() > 1 {
        pos[1..].to_vec()
    } else {
        client::default_push_refs(&repo)?
    };
    if refs.is_empty() {
        return Err(Error::Invalid("nothing to push (no refs selected)".into()));
    }
    let _span = obs::span("push");
    let rep = client::push(&repo, &remote, &refs, a.flag("force"))?;
    obs::event(
        "push_done",
        &[
            ("refs", json!(rep.refs_pushed)),
            ("objects", json!(rep.objects_sent)),
            ("bytes", json!(rep.bytes_sent)),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = format!(
        "pushed {} refs to {} ({}): {} objects, {} bytes\n",
        rep.refs_pushed.len(),
        rep.remote,
        rep.url,
        rep.objects_sent,
        rep.bytes_sent
    );
    for r in &rep.refs_pushed {
        t.push_str(&format!("  ref {r} →\n"));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

fn cmd_pull(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let pos = a.positional();
    let name = get(pos, 0, "remote name")?;
    let remote = client::get_remote(&repo, name)?;
    let filter: Option<Vec<String>> = if pos.len() > 1 {
        Some(pos[1..].to_vec())
    } else {
        None
    };
    let _span = obs::span("pull");
    let rep = client::pull(&repo, &remote, filter.as_deref())?;
    obs::event(
        "pull_done",
        &[
            ("refs_updated", json!(rep.refs_updated)),
            ("objects", json!(rep.objects_received)),
            ("bytes", json!(rep.bytes_received)),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = format!(
        "pulled from {} ({}): {} objects, {} bytes; refs updated: {}; up to date: {}\n",
        rep.remote,
        rep.url,
        rep.objects_received,
        rep.bytes_received,
        rep.refs_updated.len(),
        rep.refs_up_to_date.len()
    );
    for r in &rep.refs_updated {
        t.push_str(&format!("  ref {r} updated\n"));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

// ---------------------------------------------------------------------------
// token add/list/remove  (server-side credential management)
// ---------------------------------------------------------------------------

fn cmd_token(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["role", "token-file", "token"], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "role", "token-file", "token"])?;
    let repo = open_repo(ctx)?;
    let path = a
        .opt("token-file")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.ng().join("tokens.json"));
    let pos = a.positional();
    match pos.first().map(String::as_str) {
        Some("add") => {
            let id = get(pos, 1, "token id")?;
            let role = Role::parse(a.req("role")?)?;
            let raw = match a.opt("token") {
                Some(t) => t.to_string(),
                None => auth::generate_token()?,
            };
            let mut tf = auth::load(&path)?;
            tf.add(id, &raw, role)?;
            auth::save(&path, &tf)?;
            // The raw token is printed EXACTLY once, here.
            if ctx.json {
                return Ok(Output::Json(
                    json!({ "id": id, "role": role.as_str(), "token": raw }),
                ));
            }
            Ok(Output::Text(format!(
                "token {id} ({}) added to {}\nraw token (shown once — store it now):\n{raw}",
                role.as_str(),
                path.display()
            )))
        }
        Some("list") | Some("ls") | None => {
            let tf = auth::load(&path)?;
            if ctx.json {
                let entries: Vec<_> = tf
                    .tokens
                    .iter()
                    .map(|t| json!({ "id": t.id, "role": t.role.as_str() }))
                    .collect();
                return Ok(Output::Json(json!(entries)));
            }
            let mut t = String::new();
            if tf.tokens.is_empty() {
                t.push_str(&format!(
                    "no tokens in {} (see `newgit token add`)\n",
                    path.display()
                ));
            }
            for tok in &tf.tokens {
                t.push_str(&format!("{}\t{}\n", tok.id, tok.role.as_str()));
            }
            Ok(Output::Text(t.trim_end().to_string()))
        }
        Some("remove") | Some("rm") => {
            let id = get(pos, 1, "token id")?;
            let mut tf = auth::load(&path)?;
            if !tf.remove(id)? {
                return Err(Error::Config(format!("no token with id {id:?}")));
            }
            auth::save(&path, &tf)?;
            finish(ctx, json!({ "removed": id }), format!("token {id} removed"))
        }
        Some(other) => Err(Error::Invalid(format!(
            "unknown token subcommand {other:?}; try add|list|remove"
        ))),
    }
}

// ---------------------------------------------------------------------------
// audit show (server-side activity log)
// ---------------------------------------------------------------------------

fn cmd_audit(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["limit"], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "limit", "n"])?;
    let repo = open_repo(ctx)?;
    let limit: usize = match a.opt("limit") {
        Some(s) => s
            .parse()
            .map_err(|_| Error::Invalid(format!("--limit must be a number, got {s:?}")))?,
        None => 100,
    }
    .clamp(1, 100_000);
    let entries = crate::remote::audit::AuditLog::new(repo.ng()).tail(limit)?;
    if ctx.json {
        return Ok(Output::Json(
            json!({ "entries": entries, "count": entries.len() }),
        ));
    }
    if entries.is_empty() {
        return Ok(Output::Text(
            "audit log is empty (server activity is recorded by `newgit serve`)".into(),
        ));
    }
    let mut t = String::new();
    for e in &entries {
        t.push_str(&format!(
            "{}\t{}\t{} {}\t{}{}\n",
            e["ts_ms"].as_i64().unwrap_or(0),
            e["principal"].as_str().unwrap_or("?"),
            e["method"].as_str().unwrap_or("?"),
            e["path"].as_str().unwrap_or("?"),
            e["status"].as_u64().unwrap_or(0),
            match e["error"].as_str() {
                Some(c) => format!("\terror={c}"),
                None => String::new(),
            }
        ));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn get<'a>(pos: &'a [String], i: usize, what: &str) -> Result<&'a str> {
    pos.get(i)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing {what}")))
}

fn finish(ctx: &Ctx, data: serde_json::Value, text: String) -> Result<Output> {
    if ctx.json {
        Ok(Output::Json(data))
    } else {
        Ok(Output::Text(text))
    }
}
