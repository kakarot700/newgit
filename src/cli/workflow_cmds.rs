//! CLI commands for workflow entities (goals, changes, evidence,
//! evaluations, proposals). Shares Ctx/Output/helpers with `cli::mod`.

use serde_json::json;

use crate::cli::args::{Args, COMMON_ALIASES};
use crate::cli::{open_repo, resolve_actor, resolve_oid_arg, Ctx, Output};
use crate::error::{Error, Result};
use crate::object::types::{ChangeStatus, GoalStatus, ObjectType, ProposalState, Verdict};
use crate::object::ObjectId;
use crate::ops::workflow;
use crate::util::timefmt;

/// Resolve an entity spec: full 64-hex, or an unambiguous prefix (≥4) of a
/// chain root under refs/chains/.
pub fn resolve_entity(repo: &crate::repo::Repo, spec: &str, tag: ObjectType) -> Result<ObjectId> {
    if spec.len() == 64 {
        if let Ok(id) = ObjectId::from_hex(spec) {
            // chain must exist and match the type
            workflow::current(repo, id, tag)?;
            return Ok(id);
        }
    }
    if spec.len() < 4 || !spec.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Invalid(format!(
            "entity id must be hex (≥4 chars for prefix): {spec:?}"
        )));
    }
    let dir = repo.ng().join("refs").join("chains");
    let mut hits: Vec<String> = Vec::new();
    if dir.exists() {
        for e in std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
            let name = e
                .map_err(|err| Error::io(&dir, err))?
                .file_name()
                .to_string_lossy()
                .to_string();
            if name.starts_with(spec) {
                hits.push(name);
            }
        }
    }
    hits.sort();
    // type-filter hits
    let mut typed: Vec<ObjectId> = Vec::new();
    for h in hits {
        let id = ObjectId::from_hex(&h)?;
        if workflow::current(repo, id, tag).is_ok() {
            typed.push(id);
        }
    }
    match typed.len() {
        0 => Err(Error::Invalid(format!(
            "no {} entity matching {spec:?}",
            tag.name()
        ))),
        1 => Ok(typed[0]),
        _ => Err(Error::Invalid(format!(
            "entity prefix {spec:?} is ambiguous ({} matches)",
            typed.len()
        ))),
    }
}

/// Resolve an evidence/evaluation TARGET: entity chain (change/proposal by
/// prefix) or any stored object (snapshot) by oid/prefix.
fn resolve_target(repo: &crate::repo::Repo, spec: &str) -> Result<ObjectId> {
    // try change chain first, then proposal, then plain object
    if let Ok(id) = resolve_entity(repo, spec, ObjectType::Change) {
        return Ok(id);
    }
    if let Ok(id) = resolve_entity(repo, spec, ObjectType::Proposal) {
        return Ok(id);
    }
    resolve_oid_arg(repo, spec, "target")
}

fn parse_verdict(s: &str) -> Result<Verdict> {
    Verdict::parse(s)
}

fn oid_or_dash(o: Option<ObjectId>) -> String {
    o.map(|x| x.short()).unwrap_or_else(|| "-".into())
}

pub(crate) fn dispatch(ctx: &Ctx, entity: &str, tail: &[String]) -> Result<Output> {
    let (sub, rest) = match tail.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => {
            return Err(Error::Invalid(format!(
                "usage: newgit {entity} <subcommand>; see `newgit help`"
            )))
        }
    };
    match entity {
        "goal" => goal(ctx, sub, rest),
        "change" => change(ctx, sub, rest),
        "evidence" => evidence(ctx, sub, rest),
        "evaluation" => evaluation(ctx, sub, rest),
        "proposal" => proposal(ctx, sub, rest),
        other => Err(Error::Bug(format!("dispatch: {other}"))),
    }
}

// ─────────────────────────── goal ───────────────────────────

fn goal(ctx: &Ctx, sub: &str, rest: &[String]) -> Result<Output> {
    let repo = open_repo(ctx)?;
    match sub {
        "create" => {
            let a = Args::parse(
                rest,
                &["description", "time", "author", "author-name"],
                COMMON_ALIASES,
            )?;
            let title = a.pos_req(0, "title")?;
            let actor = resolve_actor(&repo, &a)?;
            let ts = parse_time(a.opt("time"))?;
            let id =
                workflow::goal_create(&repo, title, a.opt("description").unwrap_or(""), actor, ts)?;
            if ctx.json {
                Ok(Output::Json(json!({ "goal": id })))
            } else {
                Ok(Output::Text(format!("created goal {id}")))
            }
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "goal-id")?, ObjectType::Goal)?;
            let (head, g) = workflow::goal(&repo, id)?;
            if ctx.json {
                return Ok(Output::Json(
                    json!({ "id": id, "version": head, "goal": g }),
                ));
            }
            Ok(Output::Text(format!(
                "goal {}\n  title: {}\n  status: {}\n  creator: {}\n  created: {}\n  updated: {}\n  version: {}\n  {}",
                id,
                g.title,
                g.status.name(),
                g.creator,
                timefmt::iso8601_utc(g.created_ms),
                timefmt::iso8601_utc(g.updated_ms),
                head,
                g.description
            )))
        }
        "list" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let _ = a;
            let list = workflow::list_entities(&repo, ObjectType::Goal)?;
            if ctx.json {
                let v: Vec<_> = list
                    .iter()
                    .map(|(id, o)| json!({ "id": id, "goal": o.as_goal().unwrap() }))
                    .collect();
                return Ok(Output::Json(json!(v)));
            }
            let mut out = String::new();
            for (id, o) in &list {
                let g = o.as_goal()?;
                out.push_str(&format!(
                    "{} [{}] {}\n",
                    id.short(),
                    g.status.name(),
                    g.title
                ));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "set-status" => {
            let a = Args::parse(rest, &["time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "goal-id")?, ObjectType::Goal)?;
            let status = GoalStatus::parse(a.pos_req(1, "status")?)?;
            let actor = resolve_actor(&repo, &a)?;
            let v =
                workflow::goal_set_status(&repo, id, status, actor, parse_time(a.opt("time"))?)?;
            if ctx.json {
                Ok(Output::Json(
                    json!({ "version": v, "status": status.name() }),
                ))
            } else {
                Ok(Output::Text(format!(
                    "goal {} → {} (version {v})",
                    id.short(),
                    status.name()
                )))
            }
        }
        other => Err(Error::Invalid(format!(
            "unknown goal subcommand {other:?} (create|show|list|set-status)"
        ))),
    }
}

// ─────────────────────────── change ───────────────────────────

fn change(ctx: &Ctx, sub: &str, rest: &[String]) -> Result<Output> {
    let repo = open_repo(ctx)?;
    match sub {
        "create" => {
            let a = Args::parse(
                rest,
                &[
                    "base",
                    "result",
                    "goal",
                    "description",
                    "time",
                    "author",
                    "author-name",
                ],
                COMMON_ALIASES,
            )?;
            let title = a.pos_req(0, "title")?;
            let base = crate::repo::workspace::resolve_base(&repo, Some(a.req("base")?))?
                .ok_or_else(|| Error::Invalid("--base did not resolve".into()))?;
            let result = crate::repo::workspace::resolve_base(&repo, Some(a.req("result")?))?
                .ok_or_else(|| Error::Invalid("--result did not resolve".into()))?;
            let goal = a
                .opt("goal")
                .map(|g| resolve_entity(&repo, g, ObjectType::Goal))
                .transpose()?;
            let actor = resolve_actor(&repo, &a)?;
            let id = workflow::change_create(
                &repo,
                &workflow::ChangeInput {
                    base,
                    result,
                    author: actor,
                    goal,
                    title: title.to_string(),
                    description: a.opt("description").unwrap_or("").to_string(),
                    ts: parse_time(a.opt("time"))?,
                },
            )?;
            if ctx.json {
                Ok(Output::Json(json!({ "change": id })))
            } else {
                Ok(Output::Text(format!("created change {id}")))
            }
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "change-id")?, ObjectType::Change)?;
            let (head, c) = workflow::change(&repo, id)?;
            if ctx.json {
                return Ok(Output::Json(
                    json!({ "id": id, "version": head, "change": c }),
                ));
            }
            let mut out = format!(
                "change {}\n  title: {}\n  status: {}\n  base: {}\n  result: {}\n  goal: {}\n  author: {}\n  version: {}\n",
                id,
                c.title,
                c.status.name(),
                c.base.short(),
                c.result.short(),
                oid_or_dash(c.goal),
                c.author.short(),
                head
            );
            for e in &c.evidence {
                out.push_str(&format!("  evidence: {e}\n"));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "list" => {
            let a = Args::parse(rest, &["goal"], COMMON_ALIASES)?;
            let goal_filter = a
                .opt("goal")
                .map(|g| resolve_entity(&repo, g, ObjectType::Goal))
                .transpose()?;
            let list = workflow::list_entities(&repo, ObjectType::Change)?;
            let mut rows = Vec::new();
            for (id, o) in &list {
                let c = o.as_change()?;
                if let Some(g) = goal_filter {
                    if c.goal != Some(g) {
                        continue;
                    }
                }
                rows.push((id, c));
            }
            if ctx.json {
                let v: Vec<_> = rows
                    .iter()
                    .map(|(id, c)| json!({ "id": id, "change": c }))
                    .collect();
                return Ok(Output::Json(json!(v)));
            }
            let mut out = String::new();
            for (id, c) in &rows {
                out.push_str(&format!(
                    "{} [{}] {} (goal {})\n",
                    id.short(),
                    c.status.name(),
                    c.title,
                    oid_or_dash(c.goal)
                ));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "set-status" => {
            let a = Args::parse(rest, &["time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "change-id")?, ObjectType::Change)?;
            let status = ChangeStatus::parse(a.pos_req(1, "status")?)?;
            let actor = resolve_actor(&repo, &a)?;
            let v =
                workflow::change_set_status(&repo, id, status, actor, parse_time(a.opt("time"))?)?;
            if ctx.json {
                Ok(Output::Json(
                    json!({ "version": v, "status": status.name() }),
                ))
            } else {
                Ok(Output::Text(format!(
                    "change {} → {} (version {v})",
                    id.short(),
                    status.name()
                )))
            }
        }
        "attach-evidence" => {
            let a = Args::parse(rest, &["time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "change-id")?, ObjectType::Change)?;
            let ev = resolve_oid_arg(&repo, a.pos_req(1, "evidence-oid")?, "evidence")?;
            let actor = resolve_actor(&repo, &a)?;
            let v =
                workflow::change_attach_evidence(&repo, id, ev, actor, parse_time(a.opt("time"))?)?;
            if ctx.json {
                Ok(Output::Json(json!({ "version": v })))
            } else {
                Ok(Output::Text(format!(
                    "attached evidence {} to change {} (version {v})",
                    ev.short(),
                    id.short()
                )))
            }
        }
        other => Err(Error::Invalid(format!(
            "unknown change subcommand {other:?} (create|show|list|set-status|attach-evidence)"
        ))),
    }
}

// ─────────────────────────── evidence ───────────────────────────

fn evidence(ctx: &Ctx, sub: &str, rest: &[String]) -> Result<Output> {
    let repo = open_repo(ctx)?;
    match sub {
        "record" => {
            // evidence record [--kind K] [--target T] [-w ws] -- cmd args...
            let a = Args::parse(
                rest,
                &[
                    "kind",
                    "target",
                    "workspace",
                    "time",
                    "author",
                    "author-name",
                ],
                COMMON_ALIASES,
            )?;
            let argv: Vec<String> = a.positional().to_vec();
            if argv.is_empty() {
                return Err(Error::Invalid(
                    "usage: newgit evidence record [--kind K] [--target id] [-w ws] -- <command> [args…]".into(),
                ));
            }
            let kind = a.opt("kind").unwrap_or("command");
            let ws = a.opt("workspace").unwrap_or(crate::repo::workspace::MAIN);
            let info = crate::repo::workspace::info(&repo, ws)?;
            let target = a
                .opt("target")
                .map(|t| resolve_target(&repo, t))
                .transpose()?;
            let actor = resolve_actor(&repo, &a)?;
            let rep = workflow::evidence_record(
                &repo,
                actor,
                target,
                kind,
                &info.dir,
                &argv,
                parse_time(a.opt("time"))?,
            )?;
            if ctx.json {
                return Ok(Output::Json(
                    serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
                ));
            }
            Ok(Output::Text(format!(
                "evidence {} recorded: verdict={} exit={} duration={}ms bytes={}{}",
                rep.evidence.short(),
                rep.verdict.name(),
                rep.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                rep.duration_ms,
                rep.output_bytes,
                if rep.truncated { " (truncated)" } else { "" }
            )))
        }
        "add" => {
            // manual/opinion evidence — deterministic defaults FALSE
            let a = Args::parse(
                rest,
                &[
                    "kind",
                    "verdict",
                    "target",
                    "tool",
                    "tool-version",
                    "command",
                    "output",
                    "metric",
                    "time",
                    "author",
                    "author-name",
                ],
                COMMON_ALIASES,
            )?;
            let kind = a.req("kind")?.to_string();
            let verdict = parse_verdict(a.req("verdict")?)?;
            let target = a
                .opt("target")
                .map(|t| resolve_target(&repo, t))
                .transpose()?;
            let output = a
                .opt("output")
                .map(|f| repo.objects.put_blob_from_file(std::path::Path::new(f)))
                .transpose()?;
            let actor = resolve_actor(&repo, &a)?;
            let mut metrics = std::collections::BTreeMap::new();
            if let Some(m) = a.opt("metric") {
                for part in m.split(',') {
                    if let Some((k, v)) = part.split_once('=') {
                        metrics.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
            let id = workflow::evidence_add(
                &repo,
                &workflow::EvidenceInput {
                    producer: actor,
                    target,
                    kind,
                    verdict,
                    deterministic: a.flag("deterministic"),
                    tool: a.opt("tool").unwrap_or("").to_string(),
                    tool_version: a.opt("tool-version").unwrap_or("").to_string(),
                    command: a.opt("command").unwrap_or("").to_string(),
                    output,
                    metrics,
                    ts: parse_time(a.opt("time"))?,
                },
            )?;
            if ctx.json {
                Ok(Output::Json(json!({ "evidence": id })))
            } else {
                Ok(Output::Text(format!("added evidence {id}")))
            }
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let oid = resolve_oid_arg(&repo, a.pos_req(0, "evidence-oid")?, "evidence")?;
            let obj = repo.objects.get(&oid)?;
            let e = obj.as_evidence()?.clone();
            if ctx.json {
                return Ok(Output::Json(json!({ "oid": oid, "evidence": e })));
            }
            let mut out = format!(
                "evidence {}\n  kind: {}\n  verdict: {}\n  deterministic: {}\n  producer: {}\n  target: {}\n  tool: {} {}\n  command: {}\n  output: {}\n  created: {}\n",
                oid,
                e.kind,
                e.verdict.name(),
                e.deterministic,
                e.producer.short(),
                oid_or_dash(e.target),
                e.tool,
                e.tool_version,
                e.command,
                oid_or_dash(e.output),
                timefmt::iso8601_utc(e.created_ms)
            );
            for (k, v) in &e.metrics {
                out.push_str(&format!("  {k} = {v}\n"));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        other => Err(Error::Invalid(format!(
            "unknown evidence subcommand {other:?} (record|add|show)"
        ))),
    }
}

// ─────────────────────────── evaluation ───────────────────────────

fn evaluation(ctx: &Ctx, sub: &str, rest: &[String]) -> Result<Output> {
    let repo = open_repo(ctx)?;
    match sub {
        "create" => {
            let a = Args::parse(
                rest,
                &[
                    "target",
                    "verdict",
                    "dimension",
                    "time",
                    "author",
                    "author-name",
                ],
                COMMON_ALIASES,
            )?;
            let target = resolve_target(&repo, a.req("target")?)?;
            let verdict = parse_verdict(a.req("verdict")?)?;
            let mut dims = Vec::new();
            if let Some(d) = a.opt("dimension") {
                for part in d.split(';') {
                    // name=verdict[:note]
                    let (name, r) = part
                        .split_once('=')
                        .ok_or_else(|| Error::Invalid(format!("bad --dimension {part:?}")))?;
                    let (vs, note) = match r.split_once(':') {
                        Some((v, n)) => (v, n.to_string()),
                        None => (r, String::new()),
                    };
                    dims.push((name.trim().to_string(), parse_verdict(vs)?, note));
                }
            }
            let actor = resolve_actor(&repo, &a)?;
            let id = workflow::evaluation_add(
                &repo,
                target,
                actor,
                a.flag("ai"),
                verdict,
                dims,
                parse_time(a.opt("time"))?,
            )?;
            if ctx.json {
                Ok(Output::Json(json!({ "evaluation": id })))
            } else {
                Ok(Output::Text(format!(
                    "added evaluation {id}{}",
                    if a.flag("ai") { " (ai-generated)" } else { "" }
                )))
            }
        }
        "from-evidence" => {
            let a = Args::parse(rest, &["time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "change-id")?, ObjectType::Change)?;
            let actor = resolve_actor(&repo, &a)?;
            let ev =
                workflow::evaluation_from_evidence(&repo, id, actor, parse_time(a.opt("time"))?)?;
            let obj = repo.objects.get(&ev)?;
            let e = obj.as_evaluation()?;
            if ctx.json {
                return Ok(Output::Json(json!({ "evaluation": ev, "data": e })));
            }
            Ok(Output::Text(format!(
                "evaluation {ev}: verdict={} (aggregated from {} evidence)",
                e.verdict.name(),
                e.dimensions.len()
            )))
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let oid = resolve_oid_arg(&repo, a.pos_req(0, "evaluation-oid")?, "evaluation")?;
            let obj = repo.objects.get(&oid)?;
            let e = obj.as_evaluation()?.clone();
            if ctx.json {
                return Ok(Output::Json(json!({ "oid": oid, "evaluation": e })));
            }
            let mut out = format!(
                "evaluation {}\n  target: {}\n  evaluator: {}\n  ai_generated: {}\n  verdict: {}\n  created: {}\n",
                oid,
                e.target,
                e.evaluator.short(),
                e.ai_generated,
                e.verdict.name(),
                timefmt::iso8601_utc(e.created_ms)
            );
            for (n, v, note) in &e.dimensions {
                out.push_str(&format!("  {n}: {} {note}\n", v.name()));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        other => Err(Error::Invalid(format!(
            "unknown evaluation subcommand {other:?} (create|from-evidence|show)"
        ))),
    }
}

// ─────────────────────────── proposal ───────────────────────────

fn proposal(ctx: &Ctx, sub: &str, rest: &[String]) -> Result<Output> {
    let repo = open_repo(ctx)?;
    match sub {
        "create" => {
            let a = Args::parse(
                rest,
                &["change", "rationale", "base", "evidence", "depends", "time", "author", "author-name"],
                COMMON_ALIASES,
            )?;
            let title = a.pos_req(0, "title")?;
            let change_id = resolve_entity(&repo, a.req("change")?, ObjectType::Change)?;
            let base = match a.opt("base") {
                Some(s) => crate::repo::workspace::resolve_base(&repo, Some(s))?
                    .ok_or_else(|| Error::Invalid("--base did not resolve".into()))?,
                None => {
                    // default: the change's base snapshot
                    let (_, c) = workflow::change(&repo, change_id)?;
                    c.base
                }
            };
            let mut ev = Vec::new();
            if let Some(list) = a.opt("evidence") {
                for part in list.split(',') {
                    if !part.is_empty() {
                        ev.push(resolve_oid_arg(&repo, part, "evidence")?);
                    }
                }
            }
            let mut deps = Vec::new();
            if let Some(list) = a.opt("depends") {
                for part in list.split(',') {
                    if !part.is_empty() {
                        deps.push(resolve_entity(&repo, part, ObjectType::Proposal)?);
                    }
                }
            }
            let actor = resolve_actor(&repo, &a)?;
            let id = workflow::proposal_create(
                &repo,
                &workflow::ProposalInput {
                    change: change_id,
                    title: title.to_string(),
                    rationale: a.opt("rationale").unwrap_or("").to_string(),
                    author: actor,
                    base,
                    evidence: ev,
                    depends_on: deps,
                    ts: parse_time(a.opt("time"))?,
                },
            )?;
            if ctx.json {
                Ok(Output::Json(json!({ "proposal": id })))
            } else {
                Ok(Output::Text(format!("created proposal {id}")))
            }
        }
        "show" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "proposal-id")?, ObjectType::Proposal)?;
            let (head, p) = workflow::proposal(&repo, id)?;
            if ctx.json {
                return Ok(Output::Json(json!({ "id": id, "version": head, "proposal": p })));
            }
            let mut out = format!(
                "proposal {}\n  title: {}\n  state: {}\n  change: {}\n  base: {}\n  author: {}\n  version: {}\n",
                id,
                p.title,
                p.state.name(),
                p.change.short(),
                p.base.short(),
                p.author.short(),
                head
            );
            for (ap, ts) in &p.approvals {
                out.push_str(&format!("  approval: {} at {}\n", ap.short(), timefmt::iso8601_utc(*ts)));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "list" => {
            let a = Args::parse(rest, &[], COMMON_ALIASES)?;
            let _ = a;
            let list = workflow::list_entities(&repo, ObjectType::Proposal)?;
            if ctx.json {
                let v: Vec<_> = list
                    .iter()
                    .map(|(id, o)| json!({ "id": id, "proposal": o.as_proposal().unwrap() }))
                    .collect();
                return Ok(Output::Json(json!(v)));
            }
            let mut out = String::new();
            for (id, o) in &list {
                let p = o.as_proposal()?;
                out.push_str(&format!(
                    "{} [{}] {} (change {})\n",
                    id.short(),
                    p.state.name(),
                    p.title,
                    p.change.short()
                ));
            }
            Ok(Output::Text(out.trim_end().to_string()))
        }
        "approve" | "reject" | "close" => {
            let a = Args::parse(rest, &["time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "proposal-id")?, ObjectType::Proposal)?;
            let to = match sub {
                "approve" => ProposalState::Approved,
                "reject" => ProposalState::Rejected,
                _ => ProposalState::Closed,
            };
            let actor = resolve_actor(&repo, &a)?;
            let v = workflow::proposal_transition(&repo, id, to, actor, parse_time(a.opt("time"))?)?;
            if ctx.json {
                Ok(Output::Json(json!({ "version": v, "state": to.name() })))
            } else {
                Ok(Output::Text(format!(
                    "proposal {} → {} (version {v})",
                    id.short(),
                    to.name()
                )))
            }
        }
        "integrate" => {
            let a = Args::parse(rest, &["workspace", "time", "author", "author-name"], COMMON_ALIASES)?;
            let id = resolve_entity(&repo, a.pos_req(0, "proposal-id")?, ObjectType::Proposal)?;
            let ws = a.opt("workspace").unwrap_or(crate::repo::workspace::MAIN);
            let actor = resolve_actor(&repo, &a)?;
            let rep = workflow::proposal_integrate(&repo, id, ws, actor, parse_time(a.opt("time"))?)?;
            if ctx.json {
                return Ok(Output::Json(
                    serde_json::to_value(&rep).map_err(|e| Error::Bug(e.to_string()))?,
                ));
            }
            let result_line = match &rep.integration {
                crate::ops::integrate::IntegrateOutcome::UpToDate { position } => {
                    format!("up to date ({})", position.short())
                }
                crate::ops::integrate::IntegrateOutcome::FastForward { from, to } => {
                    format!(
                        "fast-forward {} → {}",
                        from.map(|f| f.short()).unwrap_or_else(|| "unborn".into()),
                        to.short()
                    )
                }
                crate::ops::integrate::IntegrateOutcome::Merged { oid, entries, .. } => {
                    format!("merge snapshot {} ({entries} files)", oid.short())
                }
            };
            Ok(Output::Text(format!(
                "proposal {} integrated into {ws}\n  proposal version: {}\n  change version: {}\n  result: {result_line}",
                id.short(),
                rep.proposal_version.short(),
                rep.change_version.short(),
            )))
        }
        other => Err(Error::Invalid(format!(
            "unknown proposal subcommand {other:?} (create|show|list|approve|reject|close|integrate)"
        ))),
    }
}

fn parse_time(s: Option<&str>) -> Result<Option<i64>> {
    s.map(|v| {
        v.parse::<i64>()
            .map_err(|_| Error::Invalid(format!("bad --time {v:?} (unix millis)")))
    })
    .transpose()
}
