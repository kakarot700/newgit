//! NewGit CLI: dispatch, output envelopes, and command implementations.
//!
//! Conventions (docs/CLI.md):
//! * every command supports `--json` (machine-readable envelope),
//! * exit codes are stable (`src/error.rs::exit_code`),
//! * errors are actionable: they say what to do next,
//! * no command ever prints file contents except explicit `cat --raw`,
//! * no command ever prints secrets (there are none locally; remote tokens
//!   are handled in iteration 9 with the same rule).

pub mod args;
pub mod workflow_cmds;

use std::path::{Path, PathBuf};

use args::{Args, COMMON_ALIASES};
use serde_json::{json, Value};

use crate::error::{exit_code, Error, Result};
use crate::object::types::{Actor, ActorKind, Object};
use crate::object::ObjectId;
use crate::ops::{history, snapshot as snap_op, status as status_op};
use crate::repo::workspace;
use crate::repo::Repo;
use crate::util::{base64, timefmt};
use crate::{obs, VERSION};

#[derive(Debug)]
pub struct Ctx {
    pub json: bool,
    pub repo: Option<PathBuf>,
}

pub(crate) enum Output {
    Text(String),
    Json(Value),
    Raw(Vec<u8>),
}

const GLOBAL_FLAGS: &[&str] = &["json", "debug", "repo"];

pub fn run(argv: Vec<String>) -> i32 {
    // Extract globals anywhere in argv.
    let mut json = false;
    let mut debug = false;
    let mut repo: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 1;
    let tokens = &argv[..];
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--json" => json = true,
            "--debug" => debug = true,
            "--repo" | "-C" => {
                i += 1;
                match tokens.get(i) {
                    Some(p) => repo = Some(PathBuf::from(p)),
                    None => {
                        eprintln!("newgit: --repo requires a path");
                        return exit_code::USAGE;
                    }
                }
            }
            other => rest.push(other.to_string()),
        }
        i += 1;
    }
    obs::init(debug);
    let ctx = Ctx { json, repo };
    obs::event("cli_start", &[("args", json!(rest)), ("json", json!(json))]);
    let result = dispatch(&ctx, &rest);
    match result {
        Ok(out) => {
            emit(&ctx, out);
            obs::event("cli_ok", &[]);
            exit_code::OK
        }
        Err(e) => {
            obs::error_event(&e);
            if ctx.json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "category": e.category(), "message": e.to_string() } })
                );
            } else {
                eprintln!("newgit: error[{}]: {e}", e.category());
            }
            e.exit_code()
        }
    }
}

fn emit(ctx: &Ctx, out: Output) {
    match out {
        Output::Text(t) => {
            if !t.is_empty() {
                println!("{t}");
            }
        }
        Output::Json(v) => {
            let wrapped = if ctx.json {
                json!({ "ok": true, "data": v })
            } else {
                v
            };
            println!("{wrapped}");
        }
        Output::Raw(b) => {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(&b);
            let _ = lock.flush();
        }
    }
}

fn dispatch(ctx: &Ctx, argv: &[String]) -> Result<Output> {
    let (cmd, tail) = match argv.split_first() {
        Some((c, t)) => (c.as_str(), t),
        None => return Err(Error::Invalid("no command given; try `newgit help`".into())),
    };
    match cmd {
        "version" | "--version" | "-V" => cmd_version(ctx),
        "help" | "--help" | "-h" => cmd_help(tail),
        "init" => cmd_init(ctx, tail),
        "status" => cmd_status(ctx, tail),
        "snapshot" => cmd_snapshot(ctx, tail),
        "history" | "log" => cmd_history(ctx, tail),
        "cat" | "cat-object" => cmd_cat(ctx, tail),
        "hash-object" => cmd_hash_object(ctx, tail),
        "workspace" | "ws" => cmd_workspace(ctx, tail),
        "diff" => cmd_diff(ctx, tail),
        "integrate" | "merge" => cmd_integrate(ctx, tail),
        "merge-tree" => cmd_merge_tree(ctx, tail),
        "rollback" => cmd_rollback(ctx, tail),
        "checkout" => cmd_checkout(ctx, tail),
        "actor" => cmd_actor(ctx, tail),
        "config" => cmd_config(ctx, tail),
        "import-git" => cmd_import_git(ctx, tail),
        "export-git" => cmd_export_git(ctx, tail),
        "verify" | "fsck" => cmd_verify(ctx, tail),
        "gc" => cmd_gc(ctx, tail),
        "recover" => cmd_recover(ctx, tail),
        "goal" | "change" | "evidence" | "evaluation" | "proposal" => {
            workflow_cmds::dispatch(ctx, cmd, tail)
        }
        other => Err(Error::Invalid(format!(
            "unknown command {other:?}; try `newgit help`"
        ))),
    }
}

pub(crate) fn open_repo(ctx: &Ctx) -> Result<Repo> {
    match &ctx.repo {
        Some(p) => Repo::open(p),
        None => {
            let cwd = std::env::current_dir().map_err(Error::from)?;
            Repo::discover(&cwd)
        }
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn cmd_version(ctx: &Ctx) -> Result<Output> {
    if ctx.json {
        Ok(Output::Json(json!({ "version": VERSION })))
    } else {
        Ok(Output::Text(format!("newgit {VERSION}")))
    }
}

fn cmd_help(tail: &[String]) -> Result<Output> {
    let topic = tail.first().map(String::as_str);
    let text = match topic {
        None | Some("help") => HELP_MAIN,
        Some("workspace") => HELP_WORKSPACE,
        Some("snapshot") => HELP_SNAPSHOT,
        Some(t) => {
            return Err(Error::Invalid(format!(
                "no help topic {t:?}; try `newgit help`"
            )))
        }
    };
    Ok(Output::Text(text.to_string()))
}

const HELP_MAIN: &str = "\
newgit — agent-native version control

Usage: newgit [--repo <path>] [--json] [--debug] <command> [args]

Core:
  init [<dir>]                 create a repository
  status [-w <ws>] [--all]     compare workspace against its position
  snapshot -m <msg> [-w <ws>]  capture the workspace as an immutable snapshot
  history [--from <ref|oid>] [-n <N>]   walk snapshot history (alias: log)
  cat <oid> [--raw]            inspect an object (JSON; --raw dumps blobs)
  hash-object <file> [--write] compute (and optionally store) a blob id

Diff & merge:
  diff [<a> [<b>]] [--name-only|--json|--context N|--no-renames|--exit-code]
  integrate <snapshot|ref|ws:name> [-w <ws>] [-m <msg>] [--no-renames]
  merge-tree <ours> <theirs> [--base <b>] [--json]   (dry run; exit 5 on conflicts)
  rollback [-w <ws>] [--to <snapshot>] [-m <msg>]    (new snapshot, old tree)
  checkout [-w <ws>]                                 (resync files from position)

Workspaces:
  workspace create <name> [--base <ref|oid>]
  workspace list | show <name> | discard <name> [--force]

Workflow (goals / changes / evidence / evaluations / proposals):
  goal create <title> [--description D] | show <id> | list | set-status <id> <s>
  change create <title> --base <spec> --result <spec> [--goal <id>] [--description D]
  change show <id> | list [--goal <id>] | set-status <id> <s> | attach-evidence <id> <oid>
  evidence record [--kind K] [--target id] [-w ws] -- <cmd> [args…]
  evidence add --kind K --verdict V [--deterministic] [--target id] [--output f] [--metric k=v,…]
  evidence show <oid>
  evaluation create --target id --verdict V [--ai] [--dimension n=v:note;…]
  evaluation from-evidence <change-id> | show <oid>
  proposal create <title> --change <id> [--rationale R] [--base s] [--evidence oids] [--depends ids]
  proposal show <id> | list | approve <id> | reject <id> | close <id> | integrate <id> [-w ws]

Git interop (system git required; D-007):
  import-git <git-repo>        stream a git repo in (fast-export; atomic ref switch)
  export-git <target-dir>      stream history out (fast-import; target must be empty)

Maintenance:
  verify [--deep]              integrity check (fsck); exit 3 on errors
  gc [--dry-run] [--force-now] delete unreachable objects (reflog kept)
  recover                      apply pending crash-recovery journals

Identity:
  actor show                   show the default actor
  actor set-default --id <actor-id> [--name <display>]
  config show                  show repository configuration

Global:
  version, help [<topic>]
  --json     machine-readable envelope {ok, data|error}
  --repo/-C  operate on a repository root explicitly

Exit codes: 0 ok · 2 usage · 3 repo state · 4 race (retry) · 5 conflict ·
6 limit · 7 auth · 70 internal bug";

const HELP_WORKSPACE: &str = "\
newgit workspace — isolated areas for concurrent human/agent work

  workspace create <name> [--base <ref|oid>]
      Materialize <base> (default: HEAD) into .newgit/workspaces/<name>/files
      and create the position ref workspaces/<name>.

  workspace list
      All workspaces with positions.

  workspace show <name>
      Metadata + status summary.

  workspace discard <name> [--force]
      Delete position ref + files. Refuses without --force when the
      workspace has unsnapshotted changes.

Names: [A-Za-z0-9._-], ≤255 bytes, not '.'/'..', no trailing dot/.lock.";

const HELP_SNAPSHOT: &str = "\
newgit snapshot — capture a workspace as an immutable snapshot

  snapshot -m <message> [-w <workspace>]
           [--time <unix-ms>] [--tz <minutes>]
           [--author <actor-id> [--author-name <display>]]
           [--goal <oid>] [--change <oid>]

The workspace position ref is updated with compare-and-swap; concurrent
snapshot attempts on the same workspace fail with exit code 4 (retry).
--time/--tz exist for deterministic tests and imports; default is now/UTC.";

fn cmd_init(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(GLOBAL_FLAGS)?;
    let dir = PathBuf::from(a.pos(0).unwrap_or("."));
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let repo = Repo::init(&dir)?;
    obs::event(
        "init",
        &[("root", json!(repo.root().display().to_string()))],
    );
    if ctx.json {
        Ok(Output::Json(json!({
            "root": repo.root(),
            "ng": repo.ng(),
            "head": "refs/main (unborn)",
        })))
    } else {
        Ok(Output::Text(format!(
            "initialized newgit repository in {}",
            repo.ng().display()
        )))
    }
}

fn cmd_status(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["workspace", "limit"], COMMON_ALIASES)?;
    a.reject_unknown(&["workspace", "limit", "all", "json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN);
    let limit = if a.flag("all") {
        usize::MAX
    } else {
        a.opt("limit")
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| Error::Invalid(format!("bad --limit {v:?}")))
            })
            .transpose()?
            .unwrap_or(50)
    };
    let _span = obs::span("status");
    let st = status_op::status(&repo, ws, limit)?;
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&st).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut out = String::new();
    out.push_str(&format!(
        "workspace: {} @ {}\n",
        st.workspace,
        match st.head {
            Some(h) => h.short(),
            None => "unborn".into(),
        }
    ));
    if st.clean {
        out.push_str("clean: no changes\n");
    } else {
        for (label, list) in [
            ("added", &st.added),
            ("modified", &st.modified),
            ("deleted", &st.deleted),
        ] {
            if !list.is_empty() {
                out.push_str(&format!("{label} ({}):\n", list.len()));
                for p in list {
                    out.push_str(&format!("  {p}\n"));
                }
            }
        }
        if st.truncated {
            out.push_str("(lists truncated; use --all)\n");
        }
    }
    if st.ignored > 0 {
        out.push_str(&format!("ignored: {}\n", st.ignored));
    }
    for w in &st.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    Ok(Output::Text(out.trim_end().to_string()))
}

pub(crate) fn resolve_actor(repo: &Repo, a: &Args) -> Result<ObjectId> {
    match a.opt("author") {
        None => repo.default_actor(),
        Some(id) => {
            let name = a.opt("author-name").unwrap_or(id);
            let kind = match id.split_once(':') {
                Some(("human", _)) => ActorKind::Human,
                Some(("agent", _)) => ActorKind::Agent,
                Some(("process", _)) => ActorKind::Process,
                _ => ActorKind::Anonymous,
            };
            let actor = Actor {
                kind,
                id: id.to_string(),
                display_name: name.to_string(),
                tool: "newgit-cli".into(),
                tool_version: VERSION.into(),
                pubkey: None,
                extras: Default::default(),
            };
            repo.register_actor(&actor)
        }
    }
}

pub(crate) fn resolve_oid_arg(repo: &Repo, s: &str, what: &str) -> Result<ObjectId> {
    ObjectId::from_hex(s).or_else(|_| {
        repo.objects
            .resolve_prefix(s)
            .map_err(|e| Error::Invalid(format!("{what} {s:?}: {e}")))
    })
}

fn cmd_snapshot(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(
        tail,
        &[
            "message",
            "workspace",
            "time",
            "tz",
            "author",
            "author-name",
            "goal",
            "change",
        ],
        COMMON_ALIASES,
    )?;
    a.reject_unknown(&[
        "message",
        "workspace",
        "time",
        "tz",
        "author",
        "author-name",
        "goal",
        "change",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let message = a.req("message")?.to_string();
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN).to_string();
    let author = resolve_actor(&repo, &a)?;
    let timestamp_ms = a
        .opt("time")
        .map(|v| {
            v.parse::<i64>()
                .map_err(|_| Error::Invalid(format!("bad --time {v:?} (unix millis)")))
        })
        .transpose()?;
    let tz: i16 = a
        .opt("tz")
        .map(|v| {
            v.parse::<i16>()
                .map_err(|_| Error::Invalid(format!("bad --tz {v:?} (minutes)")))
        })
        .transpose()?
        .unwrap_or(0);
    let goal = a
        .opt("goal")
        .map(|g| {
            workflow_cmds::resolve_entity(&repo, g, crate::object::types::ObjectType::Goal)
                .map_err(|e| Error::Invalid(format!("--goal {g:?}: {e}")))
        })
        .transpose()?;
    let change = a
        .opt("change")
        .map(|c| {
            workflow_cmds::resolve_entity(&repo, c, crate::object::types::ObjectType::Change)
                .map_err(|e| Error::Invalid(format!("--change {c:?}: {e}")))
        })
        .transpose()?;
    let req = snap_op::SnapshotRequest {
        workspace: ws,
        message,
        author,
        timestamp_ms,
        tz_offset_min: tz,
        goal,
        change,
        extras: Default::default(),
    };
    let _span = obs::span("snapshot");
    let out = snap_op::snapshot(&repo, &req)?;
    obs::event(
        "snapshot_created",
        &[
            ("oid", json!(out.oid.to_hex())),
            ("workspace", json!(req.workspace)),
            ("entries", json!(out.entries)),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&out).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = format!(
        "snapshot {} on {} ({})\n  files: {} (hashed {}, reused {})",
        out.oid.short(),
        req.workspace,
        out.ref_name,
        out.entries,
        out.hashed,
        out.reused
    );
    for w in &out.warnings {
        t.push_str(&format!("\nwarning: {w}"));
    }
    Ok(Output::Text(t))
}

fn cmd_history(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(
        tail,
        &["from", "limit", "workspace", "goal"],
        COMMON_ALIASES,
    )?;
    a.reject_unknown(&[
        "from",
        "limit",
        "workspace",
        "goal",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let limit = a
        .opt("limit")
        .map(|v| {
            v.parse::<usize>()
                .map_err(|_| Error::Invalid(format!("bad --limit {v:?}")))
        })
        .transpose()?
        .unwrap_or(20);
    let from = match a.opt("from") {
        Some(spec) => Some(resolve_base_or_oid(&repo, spec)?),
        None => match a.opt("workspace") {
            Some(ws) => workspace::position(&repo, ws)?,
            None => None, // HEAD
        },
    };
    let _span = obs::span("history");
    let entries = match a.opt("goal") {
        Some(g) => {
            let id =
                workflow_cmds::resolve_entity(&repo, g, crate::object::types::ObjectType::Goal)?;
            crate::ops::workflow::history_for_goal(&repo, from, id, limit)?
        }
        None => history::history(&repo, from, limit)?,
    };
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&entries).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut out = String::new();
    for e in &entries {
        let author_short = match repo.objects.get(&e.snapshot.author) {
            Ok(Object::Actor(a)) => a.display_name,
            _ => e.snapshot.author.short(),
        };
        out.push_str(&format!(
            "{} {} {} {}\n",
            e.oid.short(),
            timefmt::iso8601_utc(e.snapshot.timestamp_ms),
            author_short,
            e.snapshot.message.lines().next().unwrap_or("")
        ));
    }
    if entries.is_empty() {
        out.push_str("(no history — unborn position)\n");
    }
    Ok(Output::Text(out.trim_end().to_string()))
}

fn resolve_base_or_oid(repo: &Repo, spec: &str) -> Result<ObjectId> {
    workspace::resolve_base(repo, Some(spec))?.ok_or_else(|| Error::RefNotFound(spec.to_string()))
}

fn cmd_cat(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["raw", "json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let spec = a.pos_req(0, "object-id")?;
    let oid = resolve_oid_arg(&repo, spec, "object")?;
    let obj = repo.objects.get(&oid)?;
    if a.flag("raw") {
        return match &obj {
            Object::Blob(b) => Ok(Output::Raw(b.clone())),
            other => Err(Error::Invalid(format!(
                "--raw is only valid for blobs (this is a {} object)",
                other.type_tag().name()
            ))),
        };
    }
    let value = match &obj {
        Object::Blob(b) => json!({
            "id": oid,
            "type": "blob",
            "size": b.len(),
            "content_base64": if ctx.json { base64::encode(b) } else { format!("<{} bytes; use --raw>", b.len()) },
        }),
        other => {
            let mut v = serde_json::to_value(other).map_err(|e| Error::Bug(e.to_string()))?;
            if let Value::Object(ref mut m) = v {
                m.insert("id".into(), json!(oid));
            }
            v
        }
    };
    Ok(Output::Json(value))
}

fn cmd_hash_object(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["write", "json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let file = Path::new(a.pos_req(0, "file")?);
    let oid = if a.flag("write") {
        repo.objects.put_blob_from_file(file)?
    } else {
        let data = std::fs::read(file).map_err(|e| Error::io(file, e))?;
        Object::Blob(data).id()
    };
    if ctx.json {
        Ok(Output::Json(
            json!({ "oid": oid, "wrote": a.flag("write") }),
        ))
    } else {
        Ok(Output::Text(oid.to_hex()))
    }
}

fn cmd_workspace(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let (sub, rest) =
        match tail.split_first() {
            Some((s, r)) => (s.as_str(), r),
            None => return Err(Error::Invalid(
                "usage: newgit workspace <create|list|show|discard>; see `newgit help workspace`"
                    .into(),
            )),
        };
    let repo = open_repo(ctx)?;
    match sub {
        "create" => {
            let a = Args::parse(rest, &["base", "author", "author-name"], COMMON_ALIASES)?;
            a.reject_unknown(&["base", "author", "author-name", "json", "debug", "repo"])?;
            let name = a.pos_req(0, "workspace-name")?.to_string();
            workspace::check_workspace_name(&name)?;
            let base = workspace::resolve_base(&repo, a.opt("base"))?;
            let actor = resolve_actor(&repo, &a)?;
            let info = workspace::create(&repo, &name, base, actor)?;
            obs::event("workspace_created", &[("name", json!(name))]);
            if ctx.json {
                Ok(Output::Json(
                    serde_json::to_value(&info).map_err(|e| Error::Bug(e.to_string()))?,
                ))
            } else {
                Ok(Output::Text(format!(
                    "created workspace {}\n  dir: {}\n  position: {}",
                    info.name,
                    info.dir.display(),
                    info.head_oid
                        .map(|o| o.to_hex())
                        .unwrap_or_else(|| "unborn".into()),
                )))
            }
        }
        "list" | "ls" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            a.reject_unknown(&["json", "debug", "repo"])?;
            let list = workspace::list(&repo)?;
            if ctx.json {
                return Ok(Output::Json(
                    serde_json::to_value(&list).map_err(|e| Error::Bug(e.to_string()))?,
                ));
            }
            let mut out = String::new();
            for w in &list {
                out.push_str(&format!(
                    "{}{}\tposition={}\tbase={}\n",
                    if w.is_main { "*" } else { " " },
                    w.name,
                    w.head_oid
                        .map(|o| o.short())
                        .unwrap_or_else(|| "unborn".into()),
                    w.base_oid.map(|o| o.short()).unwrap_or_else(|| "-".into()),
                ));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let name = a.pos_req(0, "workspace-name")?;
            let info = workspace::info(&repo, name)?;
            let st = status_op::status(&repo, name, 10)?;
            if ctx.json {
                let mut v = serde_json::to_value(&info).map_err(|e| Error::Bug(e.to_string()))?;
                if let Value::Object(ref mut m) = v {
                    m.insert(
                        "status".into(),
                        serde_json::to_value(&st).unwrap_or_default(),
                    );
                }
                Ok(Output::Json(v))
            } else {
                Ok(Output::Text(format!(
                    "workspace: {}\n  dir: {}\n  ref: {}\n  position: {}\n  base: {}\n  clean: {}",
                    info.name,
                    info.dir.display(),
                    info.ref_name,
                    info.head_oid
                        .map(|o| o.to_hex())
                        .unwrap_or_else(|| "unborn".into()),
                    info.base_oid
                        .map(|o| o.to_hex())
                        .unwrap_or_else(|| "-".into()),
                    st.clean,
                )))
            }
        }
        "discard" | "rm" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            a.reject_unknown(&["force", "json", "debug", "repo"])?;
            let name = a.pos_req(0, "workspace-name")?;
            workspace::discard(&repo, name, a.flag("force"))?;
            obs::event("workspace_discarded", &[("name", json!(name))]);
            if ctx.json {
                Ok(Output::Json(json!({ "discarded": name })))
            } else {
                Ok(Output::Text(format!("discarded workspace {name}")))
            }
        }
        other => Err(Error::Invalid(format!(
            "unknown workspace subcommand {other:?}; see `newgit help workspace`"
        ))),
    }
}

fn cmd_diff(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["workspace", "context"], COMMON_ALIASES)?;
    a.reject_unknown(&[
        "workspace",
        "context",
        "name-only",
        "no-renames",
        "exit-code",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN);
    let mut opts = crate::diff::DiffOpts::default();
    if a.flag("no-renames") {
        opts.detect_renames = false;
    }
    if let Some(c) = a.opt("context") {
        opts.context = c
            .parse()
            .map_err(|_| Error::Invalid(format!("bad --context {c:?}")))?;
    }
    let pos = a.positional();
    if pos.len() > 2 {
        return Err(Error::Invalid(
            "usage: newgit diff [<a> [<b>]] (specs: ref, oid, ws:<name>; omit b for live workspace)".into(),
        ));
    }
    // resolve sides to trees
    let (old_tree, new_tree, label_a, label_b) = match pos.len() {
        0 => {
            // position vs live workspace
            let head = workspace::position(&repo, ws)?;
            let old_tree = match head {
                Some(h) => crate::diff::snapshot_or_tree_to_root(&repo, h)?,
                None => repo
                    .objects
                    .put(&Object::Tree(crate::object::types::Tree::empty()))?,
            };
            let cap = crate::ops::snapshot::capture_tree(&repo, ws, false)?;
            (
                old_tree,
                cap.root,
                format!("position[{ws}]"),
                format!("workspace[{ws}]"),
            )
        }
        1 => {
            let old_tree = crate::diff::resolve_tree(&repo, &pos[0])?;
            let cap = crate::ops::snapshot::capture_tree(&repo, ws, false)?;
            (
                old_tree,
                cap.root,
                pos[0].clone(),
                format!("workspace[{ws}]"),
            )
        }
        _ => {
            let old_tree = crate::diff::resolve_tree(&repo, &pos[0])?;
            let new_tree = crate::diff::resolve_tree(&repo, &pos[1])?;
            (old_tree, new_tree, pos[0].clone(), pos[1].clone())
        }
    };
    let _span = obs::span("diff");
    let td = crate::diff::diff_trees(&repo, old_tree, new_tree, &opts)?;

    if a.flag("name-only") {
        if ctx.json {
            let names: Vec<&str> = td.files.iter().map(|f| f.path.as_str()).collect();
            return Ok(Output::Json(json!({ "files": names })));
        }
        let mut out = String::new();
        for f in &td.files {
            out.push_str(&f.path);
            out.push('\n');
        }
        return Ok(if td.files.is_empty() {
            Output::Text(String::new())
        } else {
            Output::Text(out.trim_end().to_string())
        });
    }

    // content diffs for modified/added/deleted/renamed-with-changes
    let mut contents: Vec<(usize, crate::diff::ContentDiff)> = Vec::new();
    for (i, f) in td.files.iter().enumerate() {
        if f.binary || f.old_oid == f.new_oid {
            continue;
        }
        let cd = crate::diff::diff_blob_content(&repo, f.old_oid, f.new_oid, &opts)?;
        contents.push((i, cd));
    }

    if ctx.json {
        let mut files_json = Vec::new();
        for (i, f) in td.files.iter().enumerate() {
            let mut v = serde_json::to_value(f).map_err(|e| Error::Bug(e.to_string()))?;
            if let Some((_, cd)) = contents.iter().find(|(j, _)| *j == i) {
                if !cd.binary {
                    let hs = crate::diff::render::hunks(
                        &cd.a_lines,
                        &cd.b_lines,
                        &cd.ops2,
                        opts.context,
                    );
                    v.as_object_mut().unwrap().insert(
                        "hunks".into(),
                        serde_json::to_value(&hs).unwrap_or_default(),
                    );
                    if cd.ops.is_none() {
                        v.as_object_mut()
                            .unwrap()
                            .insert("edit_distance_capped".into(), json!(true));
                    }
                }
            }
            files_json.push(v);
        }
        return Ok(Output::Json(json!({
            "a": label_a,
            "b": label_b,
            "rename_detection": td.rename_detection,
            "files": files_json,
        })));
    }

    let mut out = String::new();
    for (i, f) in td.files.iter().enumerate() {
        out.push_str(&crate::diff::render::file_header(f));
        if let Some((_, cd)) = contents.iter().find(|(j, _)| *j == i) {
            out.push_str(&crate::diff::render::render_content(f, cd, opts.context));
        }
    }
    if td.is_empty() {
        if a.flag("exit-code") {
            return Ok(Output::Text(String::new()));
        }
        return Ok(Output::Text(String::new()));
    }
    if a.flag("exit-code") {
        // print then exit 1 (git-compatible); handled via sentinel error
        print!("{}", out);
        std::process::exit(1);
    }
    Ok(Output::Text(out.trim_end().to_string()))
}

fn merge_opts_from(a: &Args) -> Result<crate::merge::MergeOpts> {
    let mut opts = crate::merge::MergeOpts::default();
    if a.flag("no-renames") {
        opts.track_renames = false;
    }
    if let Some(d) = a.opt("max-edit-distance") {
        opts.max_edit_distance = d
            .parse()
            .map_err(|_| Error::Invalid(format!("bad --max-edit-distance {d:?}")))?;
    }
    Ok(opts)
}

fn cmd_integrate(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(
        tail,
        &[
            "workspace",
            "message",
            "time",
            "author",
            "author-name",
            "max-edit-distance",
        ],
        COMMON_ALIASES,
    )?;
    a.reject_unknown(&[
        "workspace",
        "message",
        "time",
        "author",
        "author-name",
        "no-renames",
        "max-edit-distance",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let other_spec = a.pos_req(0, "snapshot-or-ref")?;
    let other = crate::repo::workspace::resolve_base(&repo, Some(other_spec))?
        .ok_or_else(|| Error::Invalid(format!("cannot resolve {other_spec:?}")))?;
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN).to_string();
    let author = resolve_actor(&repo, &a)?;
    let req = crate::ops::integrate::IntegrateRequest {
        workspace: ws.clone(),
        other,
        message: a.opt("message").map(String::from),
        author,
        timestamp_ms: a
            .opt("time")
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| Error::Invalid(format!("bad --time {v:?}")))
            })
            .transpose()?,
        merge_opts: merge_opts_from(&a)?,
    };
    let _span = obs::span("integrate");
    let out = crate::ops::integrate::integrate(&repo, &req)?;
    obs::event(
        "integrated",
        &[("workspace", json!(ws)), ("other", json!(other.to_hex()))],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&out).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let t = match &out {
        crate::ops::integrate::IntegrateOutcome::UpToDate { position } => {
            format!("already up to date ({})", position.short())
        }
        crate::ops::integrate::IntegrateOutcome::FastForward { from, to } => format!(
            "fast-forward {} → {} on {ws}",
            from.map(|f| f.short()).unwrap_or_else(|| "unborn".into()),
            to.short()
        ),
        crate::ops::integrate::IntegrateOutcome::Merged {
            oid,
            entries,
            renames,
            ..
        } => format!(
            "merged {} into {ws} ({entries} files, {} renames)",
            oid.short(),
            renames.len()
        ),
    };
    Ok(Output::Text(t))
}

fn cmd_merge_tree(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["base", "max-edit-distance"], COMMON_ALIASES)?;
    a.reject_unknown(&[
        "base",
        "no-renames",
        "max-edit-distance",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let ours = a.pos_req(0, "ours")?;
    let theirs = a.pos_req(1, "theirs")?;
    let opts = merge_opts_from(&a)?;
    let _span = obs::span("merge_tree");
    let out = crate::ops::integrate::merge_tree_dry(&repo, ours, theirs, a.opt("base"), &opts)?;
    if !out.clean {
        // deterministic exit code for scripts: 5 = conflicts (both formats)
        if ctx.json {
            let v = serde_json::to_value(&out).map_err(|e| Error::Bug(e.to_string()))?;
            println!("{}", json!({ "ok": true, "data": v }));
        } else {
            let mut t = String::new();
            t.push_str(&format!(
                "merge {} × {} — NOT CLEAN ({} conflicts)\n  tree: {}\n",
                ours,
                theirs,
                out.conflicts.len(),
                out.root
            ));
            for (from, to) in &out.renames {
                t.push_str(&format!("  rename: {from} → {to}\n"));
            }
            for c in &out.conflicts {
                t.push_str(&format!(
                    "  conflict: {}\n",
                    serde_json::to_string(c).unwrap_or_default()
                ));
            }
            print!("{t}");
        }
        std::process::exit(exit_code::CONFLICT);
    }
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&out).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = String::new();
    t.push_str(&format!(
        "merge {} × {}\n  clean: true\n  tree: {}\n  entries: {}\n",
        ours, theirs, out.root, out.entries
    ));
    for (from, to) in &out.renames {
        t.push_str(&format!("  rename: {from} → {to}\n"));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

fn cmd_rollback(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(
        tail,
        &[
            "workspace",
            "to",
            "message",
            "time",
            "author",
            "author-name",
        ],
        COMMON_ALIASES,
    )?;
    a.reject_unknown(&[
        "workspace",
        "to",
        "message",
        "time",
        "author",
        "author-name",
        "json",
        "debug",
        "repo",
    ])?;
    let repo = open_repo(ctx)?;
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN).to_string();
    let author = resolve_actor(&repo, &a)?;
    let target = a
        .opt("to")
        .map(|s| {
            crate::repo::workspace::resolve_base(&repo, Some(s))?
                .ok_or_else(|| Error::Invalid(format!("cannot resolve --to {s:?}")))
        })
        .transpose()?;
    let req = crate::ops::integrate::RollbackRequest {
        workspace: ws.clone(),
        target,
        message: a.opt("message").map(String::from),
        author,
        timestamp_ms: a
            .opt("time")
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| Error::Invalid(format!("bad --time {v:?}")))
            })
            .transpose()?,
    };
    let _span = obs::span("rollback");
    let oid = crate::ops::integrate::rollback(&repo, &req)?;
    if ctx.json {
        return Ok(Output::Json(json!({ "oid": oid, "workspace": ws })));
    }
    Ok(Output::Text(format!(
        "rolled back {ws} to a new snapshot {oid}"
    )))
}

fn cmd_checkout(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &["workspace"], COMMON_ALIASES)?;
    a.reject_unknown(&["workspace", "json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let ws = a.opt("workspace").unwrap_or(workspace::MAIN).to_string();
    let _span = obs::span("checkout");
    let rep = crate::ops::integrate::checkout_position(&repo, &ws)?;
    if ctx.json {
        return Ok(Output::Json(json!({
            "workspace": ws,
            "files": rep.files,
            "symlinks": rep.symlinks,
            "bytes": rep.bytes,
        })));
    }
    Ok(Output::Text(format!(
        "checked out {ws}: {} files, {} symlinks, {} bytes",
        rep.files, rep.symlinks, rep.bytes
    )))
}

fn cmd_actor(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let (sub, rest) = match tail.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => {
            return Err(Error::Invalid(
                "usage: newgit actor <show|set-default>".into(),
            ))
        }
    };
    let repo = open_repo(ctx)?;
    match sub {
        "show" => {
            let oid = repo.default_actor()?;
            let obj = repo.objects.get(&oid)?;
            let actor = obj.as_actor()?.clone();
            if ctx.json {
                Ok(Output::Json(json!({ "oid": oid, "actor": actor })))
            } else {
                Ok(Output::Text(format!(
                    "{}\n  kind: {}\n  id: {}\n  tool: {} {}",
                    oid,
                    actor.kind.name(),
                    actor.id,
                    actor.tool,
                    actor.tool_version
                )))
            }
        }
        "set-default" => {
            let a = Args::parse(rest, &["id", "name"], COMMON_ALIASES)?;
            let id = a.req("id")?.to_string();
            let name = a.opt("name").unwrap_or(&id).to_string();
            let mut cfg = repo.config.clone();
            cfg.default_actor_id = Some(id.clone());
            cfg.default_actor_name = Some(name.clone());
            cfg.save(&repo.ng().join("config"))?;
            // register immediately so the actor object exists
            let kind = match id.split_once(':') {
                Some(("human", _)) => ActorKind::Human,
                Some(("agent", _)) => ActorKind::Agent,
                Some(("process", _)) => ActorKind::Process,
                _ => ActorKind::Anonymous,
            };
            let actor = Actor {
                kind,
                id,
                display_name: name,
                tool: "newgit-cli".into(),
                tool_version: VERSION.into(),
                pubkey: None,
                extras: Default::default(),
            };
            let oid = repo.register_actor(&actor)?;
            if ctx.json {
                Ok(Output::Json(json!({ "oid": oid })))
            } else {
                Ok(Output::Text(format!("default actor set: {oid}")))
            }
        }
        other => Err(Error::Invalid(format!(
            "unknown actor subcommand {other:?}"
        ))),
    }
}

fn cmd_config(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let (sub, rest) = match tail.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => return Err(Error::Invalid("usage: newgit config show".into())),
    };
    let repo = open_repo(ctx)?;
    let a = Args::parse(rest, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    match sub {
        "show" => {
            if ctx.json {
                let mut m = serde_json::Map::new();
                for (k, v) in repo.config.limits.to_map() {
                    m.insert(k, json!(v));
                }
                m.insert("format_version".into(), json!(repo.config.format_version));
                if let Some(id) = &repo.config.default_actor_id {
                    m.insert("default_actor_id".into(), json!(id));
                }
                if let Some(n) = &repo.config.default_actor_name {
                    m.insert("default_actor_name".into(), json!(n));
                }
                Ok(Output::Json(Value::Object(m)))
            } else {
                Ok(Output::Text(repo.config.serialize()))
            }
        }
        other => Err(Error::Invalid(format!(
            "unknown config subcommand {other:?}"
        ))),
    }
}

fn cmd_import_git(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    let path = a.pos_req(0, "git-repo-path")?;
    let repo = open_repo(ctx)?;
    let _span = obs::span("import_git");
    let rep = crate::gitio::import::import_git(&repo, std::path::Path::new(path))?;
    obs::event(
        "import_git_done",
        &[
            ("commits", json!(rep.commits)),
            ("blobs", json!(rep.blobs)),
            ("refs", json!(rep.refs_imported.len())),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = format!(
        "imported {} commits, {} blobs, {} trees, {} actors from {}\n",
        rep.commits, rep.blobs, rep.trees, rep.actors, path
    );
    for (name, oid) in &rep.refs_imported {
        t.push_str(&format!("  ref {name} → {}\n", &oid[..oid.len().min(12)]));
    }
    for name in &rep.refs_skipped {
        t.push_str(&format!("  skipped {name} (git-internal namespace)\n"));
    }
    for name in &rep.annotated_tags_stripped {
        t.push_str(&format!(
            "  annotated tag {name}: ref imported, tagger/message metadata stripped (documented)\n"
        ));
    }
    if let Some(h) = &rep.head {
        t.push_str(&format!("  HEAD → {h}\n"));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

fn cmd_export_git(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    let target = a.pos_req(0, "target-dir")?;
    let repo = open_repo(ctx)?;
    let _span = obs::span("export_git");
    let rep = crate::gitio::export::export_git(&repo, std::path::Path::new(target))?;
    obs::event(
        "export_git_done",
        &[
            ("commits", json!(rep.commits)),
            ("blobs", json!(rep.blobs)),
            ("refs", json!(rep.refs_exported.len())),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let mut t = format!(
        "exported {} commits, {} blobs to {}\n",
        rep.commits, rep.blobs, target
    );
    for (ng, g) in &rep.refs_exported {
        t.push_str(&format!("  {ng} → {g}\n"));
    }
    for name in &rep.refs_skipped {
        t.push_str(&format!("  skipped {name} (newgit-internal namespace)\n"));
    }
    if let Some(h) = &rep.head {
        t.push_str(&format!("  git HEAD → {h}\n"));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

fn cmd_verify(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "deep"])?;
    let repo = open_repo(ctx)?;
    let _span = obs::span("verify");
    let rep = crate::ops::verify::verify(
        &repo,
        &crate::ops::verify::VerifyOpts {
            deep: a.flag("deep"),
        },
    );
    obs::event(
        "verify_done",
        &[
            ("objects", json!(rep.objects_checked)),
            ("errors", json!(rep.errors())),
            ("warnings", json!(rep.warnings())),
        ],
    );
    if ctx.json {
        // always a full report; the exit code carries pass/fail
        let v = serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?;
        println!("{}", json!({ "ok": true, "data": v }));
    } else {
        let mut t = String::new();
        t.push_str(&format!(
            "checked {} objects, {} refs, {} chains, {} workspaces ({} quarantined)\n",
            rep.objects_checked,
            rep.refs_checked,
            rep.chains_checked,
            rep.workspaces_checked,
            rep.quarantined
        ));
        for i in &rep.issues {
            let sev = match i.severity {
                crate::ops::verify::Severity::Error => "error",
                crate::ops::verify::Severity::Warning => "warn ",
            };
            t.push_str(&format!("  [{sev}] {} — {}\n", i.code, i.detail));
        }
        t.push_str(&format!(
            "{} error(s), {} warning(s)\n",
            rep.errors(),
            rep.warnings()
        ));
        print!("{t}");
    }
    if !rep.ok() {
        std::process::exit(exit_code::REPO);
    }
    Ok(Output::Text(String::new()))
}

fn cmd_gc(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo", "dry-run", "force-now"])?;
    let repo = open_repo(ctx)?;
    let _span = obs::span("gc");
    let rep = crate::ops::gc::gc(
        &repo,
        &crate::ops::gc::GcOpts {
            dry_run: a.flag("dry-run"),
            force_now: a.flag("force-now"),
        },
    )?;
    obs::event(
        "gc_done",
        &[
            ("deleted", json!(rep.deleted_objects)),
            ("live", json!(rep.live_objects)),
            ("dry_run", json!(rep.dry_run)),
        ],
    );
    if ctx.json {
        return Ok(Output::Json(
            serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
        ));
    }
    let verb = if rep.dry_run {
        "would delete"
    } else {
        "deleted"
    };
    let mut t = format!(
        "gc: {verb} {} unreachable object(s) ({} bytes); {} live, {} kept young, {} kept corrupt, {} quarantined\n",
        rep.deleted_objects,
        rep.freed_bytes,
        rep.live_objects,
        rep.kept_young,
        rep.kept_corrupt,
        rep.quarantined
    );
    if rep.missing_links > 0 {
        t.push_str(&format!(
            "note: {} root/link object(s) unreadable — run `newgit verify` for details\n",
            rep.missing_links
        ));
    }
    Ok(Output::Text(t.trim_end().to_string()))
}

fn cmd_recover(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    let repo = open_repo(ctx)?;
    let _span = obs::span("recover");
    let (rep, swept) = repo.recover()?;
    if ctx.json {
        return Ok(Output::Json(json!({
            "redone": rep.redone,
            "quarantined": rep.quarantined,
            "cleaned": rep.cleaned,
            "temp_files_swept": swept,
        })));
    }
    Ok(Output::Text(format!(
        "recover: {} journal(s) redone, {} terminal journal(s) cleaned, {} object(s) quarantined, {} temp file(s) swept",
        rep.redone.len(),
        rep.cleaned,
        rep.quarantined.len(),
        swept
    )))
}
