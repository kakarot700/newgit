//! Workflow entities: Goals, Changes, Evidence, Evaluations, Proposals.
//!
//! All objects are immutable and content-addressed. Mutable *entities* are
//! version chains: the entity id is the oid of its first version; the ref
//! `chains/<id-hex>` points at the latest version and is only moved by the
//! txn engine with CAS (concurrent updates ⇒ exit 4, retry). Every new
//! version carries `extras["prev"] = <previous version oid>` so the full
//! audit trail is reconstructible from any head.
//!
//! HONESTY RULES (SECURITY_MODEL §5):
//! * `Evidence.deterministic` separates machine-recorded results from
//!   opinions; `evidence record` (runner-captured exit/output/duration) is
//!   the only path where the CLI itself sets deterministic=true.
//! * `Evaluation.ai_generated` marks AI opinions; they never masquerade as
//!   deterministic verdicts.
//! * Verdicts are never inferred from free text.

use std::collections::BTreeMap;
use std::process::Stdio;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::object::types::{
    Actor, Change, ChangeStatus, Evaluation, Evidence, Goal, GoalStatus, Object, ObjectType,
    Proposal, ProposalState, Verdict,
};
use crate::object::ObjectId;
use crate::ops::integrate::{self, IntegrateRequest};
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::VERSION;

fn chain_ref(id: ObjectId) -> String {
    format!("chains/{}", id.to_hex())
}

/// Load the CURRENT version of an entity chain, checking its type.
pub fn current(repo: &Repo, id: ObjectId, tag: ObjectType) -> Result<(ObjectId, Object)> {
    let head = repo
        .refs
        .read_opt(&chain_ref(id))?
        .ok_or(Error::NotFound(id))?;
    let obj = repo.objects.get(&head)?;
    if obj.type_tag() != tag {
        return Err(Error::Invalid(format!(
            "entity {id} is a {} object, not {}",
            obj.type_tag().name(),
            tag.name()
        )));
    }
    Ok((head, obj))
}

fn create_entity(repo: &Repo, obj: Object, actor: ObjectId, msg: &str) -> Result<ObjectId> {
    obj.validate()?;
    let oid = repo.objects.put(&obj)?;
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: chain_ref(oid),
            cas: Cas::Exactly(None),
            new: Some(oid),
            log: RefLogEntry {
                actor: Some(actor),
                ts_ms: txn::now_ms(),
                message: msg.to_string(),
            },
        }],
        repo.limits(),
    )?;
    Ok(oid)
}

/// Commit a new chain version: object write + CAS ref move in the txn.
fn commit_version(
    repo: &Repo,
    root: ObjectId,
    old_head: ObjectId,
    obj: Object,
    actor: ObjectId,
    msg: &str,
) -> Result<ObjectId> {
    obj.validate()?;
    let new_oid = repo.objects.put(&obj)?;
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: chain_ref(root),
            cas: Cas::Exactly(Some(old_head)),
            new: Some(new_oid),
            log: RefLogEntry {
                actor: Some(actor),
                ts_ms: txn::now_ms(),
                message: msg.to_string(),
            },
        }],
        repo.limits(),
    )?;
    Ok(new_oid)
}

fn with_prev(mut extras: BTreeMap<String, String>, prev: ObjectId) -> BTreeMap<String, String> {
    extras.insert("prev".to_string(), prev.to_hex());
    extras
}

/// List current versions of all chains whose head has `tag`.
pub fn list_entities(repo: &Repo, tag: ObjectType) -> Result<Vec<(ObjectId, Object)>> {
    let dir = repo.ng().join("refs").join("chains");
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    names.sort();
    if names.len() > 100_000 {
        return Err(Error::Limit("too many entity chains".into()));
    }
    for n in names {
        let Ok(head) = repo.refs.read_opt(&format!("chains/{n}")) else {
            continue;
        };
        let Some(head) = head else { continue };
        let Ok(obj) = repo.objects.get(&head) else {
            continue; // dangling head: verify reports; listing skips
        };
        if obj.type_tag() == tag {
            out.push((ObjectId::from_hex(&n)?, obj));
        }
    }
    Ok(out)
}

// ─────────────────────────── goals ───────────────────────────

pub fn goal_create(
    repo: &Repo,
    title: &str,
    description: &str,
    creator: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let now = ts.unwrap_or_else(txn::now_ms);
    let g = Goal {
        title: title.to_string(),
        description: description.to_string(),
        creator,
        status: GoalStatus::Open,
        created_ms: now,
        updated_ms: now,
        extras: BTreeMap::new(),
    };
    create_entity(repo, Object::Goal(g), creator, "goal create")
}

fn goal_transitions(s: GoalStatus) -> &'static [GoalStatus] {
    match s {
        GoalStatus::Open => &[
            GoalStatus::InProgress,
            GoalStatus::Achieved,
            GoalStatus::Abandoned,
        ],
        GoalStatus::InProgress => &[
            GoalStatus::Achieved,
            GoalStatus::Abandoned,
            GoalStatus::Open,
        ],
        GoalStatus::Achieved => &[GoalStatus::InProgress], // reopen
        GoalStatus::Abandoned => &[GoalStatus::Open],      // reopen
    }
}

pub fn goal_set_status(
    repo: &Repo,
    id: ObjectId,
    status: GoalStatus,
    actor: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let (head, obj) = current(repo, id, ObjectType::Goal)?;
    let g = obj.as_goal()?;
    if g.status == status {
        return Ok(head); // no-op
    }
    if !goal_transitions(g.status).contains(&status) {
        return Err(Error::Invalid(format!(
            "goal status transition {} → {} not allowed (allowed: {})",
            g.status.name(),
            status.name(),
            goal_transitions(g.status)
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let now = ts.unwrap_or_else(txn::now_ms);
    if now < g.updated_ms {
        return Err(Error::Invalid(
            "timestamp older than current version".into(),
        ));
    }
    let next = Goal {
        status,
        updated_ms: now,
        extras: with_prev(g.extras.clone(), head),
        ..g.clone()
    };
    commit_version(
        repo,
        id,
        head,
        Object::Goal(next),
        actor,
        &format!("goal status → {}", status.name()),
    )
}

// ─────────────────────────── changes ───────────────────────────

#[derive(Clone, Debug)]
pub struct ChangeInput {
    pub base: ObjectId,
    pub result: ObjectId,
    pub author: ObjectId,
    pub goal: Option<ObjectId>,
    pub title: String,
    pub description: String,
    pub ts: Option<i64>,
}

pub fn change_create(repo: &Repo, inp: &ChangeInput) -> Result<ObjectId> {
    let ChangeInput {
        base,
        result,
        author,
        goal,
        title,
        description,
        ts,
    } = inp;
    let (base, result, author) = (*base, *result, *author);
    let goal = *goal;
    let ts = *ts;
    // both ends must be snapshots; goal must be a goal chain
    for (o, what) in [(base, "base"), (result, "result")] {
        let obj = repo.objects.get(&o)?;
        if obj.type_tag() != ObjectType::Snapshot {
            return Err(Error::Invalid(format!(
                "change {what} must be a snapshot, got {}",
                obj.type_tag().name()
            )));
        }
    }
    if base == result {
        return Err(Error::Invalid(
            "change base and result must differ (empty change)".into(),
        ));
    }
    if let Some(g) = goal {
        current(repo, g, ObjectType::Goal)?;
    }
    let now = ts.unwrap_or_else(txn::now_ms);
    let c = Change {
        base,
        result,
        author,
        goal,
        title: title.clone(),
        description: description.clone(),
        status: ChangeStatus::Draft,
        created_ms: now,
        updated_ms: now,
        evidence: Vec::new(),
        extras: BTreeMap::new(),
    };
    create_entity(repo, Object::Change(c), author, "change create")
}

fn change_transitions(s: ChangeStatus) -> &'static [ChangeStatus] {
    match s {
        ChangeStatus::Draft => &[ChangeStatus::Tested, ChangeStatus::Abandoned],
        ChangeStatus::Tested => &[ChangeStatus::Proposed, ChangeStatus::Abandoned],
        ChangeStatus::Proposed => &[ChangeStatus::Integrated, ChangeStatus::Abandoned],
        ChangeStatus::Integrated => &[],
        ChangeStatus::Abandoned => &[],
    }
}

pub fn change_set_status(
    repo: &Repo,
    id: ObjectId,
    status: ChangeStatus,
    actor: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let (head, obj) = current(repo, id, ObjectType::Change)?;
    let c = obj.as_change()?;
    if c.status == status {
        return Ok(head);
    }
    if !change_transitions(c.status).contains(&status) {
        return Err(Error::Invalid(format!(
            "change status transition {} → {} not allowed",
            c.status.name(),
            status.name()
        )));
    }
    // honesty gate: "tested" requires at least one evidence object attached
    if status == ChangeStatus::Tested && c.evidence.is_empty() {
        return Err(Error::Invalid(
            "change has no evidence — attach evidence before marking tested \
             (see `newgit evidence record`)"
                .into(),
        ));
    }
    let now = ts.unwrap_or_else(txn::now_ms);
    let next = Change {
        status,
        updated_ms: now,
        extras: with_prev(c.extras.clone(), head),
        ..c.clone()
    };
    commit_version(
        repo,
        id,
        head,
        Object::Change(next),
        actor,
        &format!("change status → {}", status.name()),
    )
}

pub fn change_attach_evidence(
    repo: &Repo,
    id: ObjectId,
    evidence: ObjectId,
    actor: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let ev = repo.objects.get(&evidence)?;
    if ev.type_tag() != ObjectType::Evidence {
        return Err(Error::Invalid(
            "attach target must be an evidence object".into(),
        ));
    }
    let (head, obj) = current(repo, id, ObjectType::Change)?;
    let c = obj.as_change()?;
    if c.evidence.contains(&evidence) {
        return Ok(head);
    }
    let mut list = c.evidence.clone();
    list.push(evidence);
    list.sort();
    list.dedup();
    let now = ts.unwrap_or_else(txn::now_ms);
    let next = Change {
        evidence: list,
        updated_ms: now,
        extras: with_prev(c.extras.clone(), head),
        ..c.clone()
    };
    commit_version(
        repo,
        id,
        head,
        Object::Change(next),
        actor,
        "change attach evidence",
    )
}

// ─────────────────────────── evidence ───────────────────────────

#[derive(Clone, Debug)]
pub struct EvidenceInput {
    pub producer: ObjectId,
    pub target: Option<ObjectId>,
    pub kind: String,
    pub verdict: Verdict,
    pub deterministic: bool,
    pub tool: String,
    pub tool_version: String,
    pub command: String,
    pub output: Option<ObjectId>,
    pub metrics: BTreeMap<String, String>,
    pub ts: Option<i64>,
}

pub fn evidence_add(repo: &Repo, inp: &EvidenceInput) -> Result<ObjectId> {
    if let Some(t) = inp.target {
        let obj = repo.objects.get(&t)?;
        match obj.type_tag() {
            ObjectType::Change | ObjectType::Snapshot | ObjectType::Proposal => {}
            other => {
                return Err(Error::Invalid(format!(
                    "evidence target must be a change/snapshot/proposal, got {}",
                    other.name()
                )))
            }
        }
        // a change TARGET must be looked up by chain id too — accept either
        // the chain root or a concrete version oid (both are real objects).
    }
    let e = Evidence {
        producer: inp.producer,
        target: inp.target,
        kind: inp.kind.clone(),
        verdict: inp.verdict,
        deterministic: inp.deterministic,
        tool: inp.tool.clone(),
        tool_version: inp.tool_version.clone(),
        command: inp.command.clone(),
        output: inp.output,
        metrics: inp.metrics.clone(),
        created_ms: inp.ts.unwrap_or_else(txn::now_ms),
        extras: BTreeMap::new(),
    };
    // Evidence objects are append-only facts: no chain, plain put.
    e.validate()?;
    repo.objects.put(&Object::Evidence(e))
}

/// Runner-recorded evidence: NewGit itself executes `argv` (explicit user
/// action, SECURITY_MODEL §2), captures stdout+stderr (capped), duration and
/// exit code; verdict derives from the exit status — never from user text.
#[derive(Clone, Debug, Serialize)]
pub struct RecordReport {
    pub evidence: ObjectId,
    pub exit_code: Option<i32>,
    pub signal: bool,
    pub verdict: Verdict,
    pub duration_ms: u64,
    pub output_bytes: usize,
    pub truncated: bool,
}

pub fn evidence_record(
    repo: &Repo,
    producer: ObjectId,
    target: Option<ObjectId>,
    kind: &str,
    cwd: &std::path::Path,
    argv: &[String],
    ts: Option<i64>,
) -> Result<RecordReport> {
    if argv.is_empty() {
        return Err(Error::Invalid("nothing to run".into()));
    }
    let cap = repo.limits().max_blob_bytes.min(8 << 20) as usize;
    let started = std::time::Instant::now();
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::Invalid(format!("cannot run {:?}: {e}", argv[0])))?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut combined = out.stdout;
    if !combined.is_empty() && !combined.ends_with(b"\n") {
        combined.push(b'\n');
    }
    combined.extend_from_slice(b"--- stderr ---\n");
    combined.extend_from_slice(&out.stderr);
    let truncated = combined.len() > cap;
    if truncated {
        combined.truncate(cap);
        combined.extend_from_slice(b"\n[newgit: output truncated]\n");
    }
    let output_oid = repo.objects.put_blob(&combined)?;
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        out.status.signal().is_some()
    };
    #[cfg(not(unix))]
    let signal = false;
    let verdict = match out.status.code() {
        Some(0) => Verdict::Pass,
        Some(_) => Verdict::Fail,
        None => Verdict::Inconclusive, // killed by signal
    };
    let mut metrics = BTreeMap::new();
    metrics.insert("duration_ms".to_string(), duration_ms.to_string());
    if let Some(code) = out.status.code() {
        metrics.insert("exit_code".to_string(), code.to_string());
    }
    metrics.insert("output_bytes".to_string(), combined.len().to_string());
    let e = Evidence {
        producer,
        target,
        kind: kind.to_string(),
        verdict,
        deterministic: true, // recorded by the runner itself
        tool: argv[0].clone(),
        tool_version: String::new(),
        command: argv.join(" "),
        output: Some(output_oid),
        metrics,
        created_ms: ts.unwrap_or_else(txn::now_ms),
        extras: {
            let mut m = BTreeMap::new();
            m.insert("recorded_by".to_string(), format!("newgit {VERSION}"));
            if signal {
                m.insert("signal".to_string(), "true".to_string());
            }
            if truncated {
                m.insert("truncated".to_string(), "true".to_string());
            }
            m
        },
    };
    e.validate()?;
    let oid = repo.objects.put(&Object::Evidence(e))?;
    Ok(RecordReport {
        evidence: oid,
        exit_code: out.status.code(),
        signal,
        verdict,
        duration_ms,
        output_bytes: combined.len(),
        truncated,
    })
}

// ─────────────────────────── evaluations ───────────────────────────

pub fn evaluation_add(
    repo: &Repo,
    target: ObjectId,
    evaluator: ObjectId,
    ai_generated: bool,
    verdict: Verdict,
    dimensions: Vec<(String, Verdict, String)>,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let obj = repo.objects.get(&target)?;
    if obj.type_tag() != ObjectType::Change && obj.type_tag() != ObjectType::Snapshot {
        return Err(Error::Invalid(
            "evaluation target must be a change or snapshot".into(),
        ));
    }
    let mut dims = dimensions;
    dims.sort_by(|a, b| a.0.cmp(&b.0));
    let ev = Evaluation {
        target,
        evaluator,
        ai_generated,
        verdict,
        dimensions: dims,
        created_ms: ts.unwrap_or_else(txn::now_ms),
        extras: BTreeMap::new(),
    };
    ev.validate()?;
    repo.objects.put(&Object::Evaluation(ev))
}

/// Deterministic aggregation: verdict from the change's attached evidence.
/// all pass ⇒ pass; any fail ⇒ fail; else inconclusive. Dimensions mirror
/// evidence kinds. ai_generated is ALWAYS false here.
pub fn evaluation_from_evidence(
    repo: &Repo,
    change_id: ObjectId,
    evaluator: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let (_, obj) = current(repo, change_id, ObjectType::Change)?;
    let c = obj.as_change()?;
    if c.evidence.is_empty() {
        return Err(Error::Invalid("change has no evidence to aggregate".into()));
    }
    let mut dims: BTreeMap<String, (Verdict, String)> = BTreeMap::new();
    let mut any_fail = false;
    let mut all_pass = true;
    for e_oid in &c.evidence {
        let e = repo.objects.get(e_oid)?;
        let e = e.as_evidence()?;
        let note = format!(
            "{} ({})",
            if e.deterministic {
                "deterministic"
            } else {
                "opinion"
            },
            e.verdict.name()
        );
        // worst verdict per kind wins; opinions never upgrade deterministic
        let entry = dims
            .entry(e.kind.clone())
            .or_insert((Verdict::Pass, note.clone()));
        entry.0 = worse(entry.0, e.verdict);
        if e.verdict == Verdict::Fail {
            any_fail = true;
        }
        if e.verdict != Verdict::Pass {
            all_pass = false;
        }
    }
    let verdict = if any_fail {
        Verdict::Fail
    } else if all_pass {
        Verdict::Pass
    } else {
        Verdict::Inconclusive
    };
    let dimensions = dims
        .into_iter()
        .map(|(k, (v, note))| (k, v, note))
        .collect();
    evaluation_add(
        repo, // evaluation targets the change chain ROOT (stable id)
        change_id, evaluator, false, verdict, dimensions, ts,
    )
}

fn worse(a: Verdict, b: Verdict) -> Verdict {
    fn rank(v: Verdict) -> u8 {
        match v {
            Verdict::Pass => 0,
            Verdict::NotApplicable => 1,
            Verdict::Inconclusive => 2,
            Verdict::Fail => 3,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

// ─────────────────────────── proposals ───────────────────────────

#[derive(Clone, Debug)]
pub struct ProposalInput {
    pub change: ObjectId,
    pub title: String,
    pub rationale: String,
    pub author: ObjectId,
    pub base: ObjectId,
    pub evidence: Vec<ObjectId>,
    pub depends_on: Vec<ObjectId>,
    pub ts: Option<i64>,
}

pub fn proposal_create(repo: &Repo, inp: &ProposalInput) -> Result<ObjectId> {
    let (_, cobj) = current(repo, inp.change, ObjectType::Change)?;
    let c = cobj.as_change()?;
    if c.status != ChangeStatus::Proposed && c.status != ChangeStatus::Tested {
        return Err(Error::Invalid(format!(
            "change must be tested or proposed before a proposal (is {})",
            c.status.name()
        )));
    }
    for e in &inp.evidence {
        let obj = repo.objects.get(e)?;
        if obj.type_tag() != ObjectType::Evidence {
            return Err(Error::Invalid(format!(
                "proposal evidence {e} is not an evidence object"
            )));
        }
    }
    let base_snap = repo.objects.get(&inp.base)?;
    if base_snap.type_tag() != ObjectType::Snapshot {
        return Err(Error::Invalid("proposal base must be a snapshot".into()));
    }
    let mut evidence = inp.evidence.clone();
    evidence.sort();
    evidence.dedup();
    let mut depends = inp.depends_on.clone();
    depends.sort();
    depends.dedup();
    let now = inp.ts.unwrap_or_else(txn::now_ms);
    let p = Proposal {
        change: inp.change,
        title: inp.title.clone(),
        rationale: inp.rationale.clone(),
        author: inp.author,
        base: inp.base,
        evidence,
        depends_on: depends,
        approvals: Vec::new(),
        state: ProposalState::Open,
        created_ms: now,
        updated_ms: now,
        extras: BTreeMap::new(),
    };
    create_entity(repo, Object::Proposal(p), inp.author, "proposal create")
}

pub fn proposal_transition(
    repo: &Repo,
    id: ObjectId,
    to: ProposalState,
    actor: ObjectId,
    ts: Option<i64>,
) -> Result<ObjectId> {
    let (head, obj) = current(repo, id, ObjectType::Proposal)?;
    let p = obj.as_proposal()?;
    if p.state == to {
        return Ok(head);
    }
    let allowed: &[ProposalState] = match p.state {
        ProposalState::Open => &[
            ProposalState::Approved,
            ProposalState::Rejected,
            ProposalState::Closed,
        ],
        ProposalState::Approved => &[ProposalState::Integrated, ProposalState::Closed],
        ProposalState::Rejected | ProposalState::Integrated | ProposalState::Closed => &[],
    };
    if to == ProposalState::Integrated {
        return Err(Error::Invalid(
            "proposals reach 'integrated' only via `newgit proposal integrate`".into(),
        ));
    }
    if !allowed.contains(&to) {
        return Err(Error::Invalid(format!(
            "proposal transition {} → {} not allowed",
            p.state.name(),
            to.name()
        )));
    }
    let now = ts.unwrap_or_else(txn::now_ms);
    let mut approvals = p.approvals.clone();
    if to == ProposalState::Approved {
        approvals.push((actor, now));
        approvals.sort();
        approvals.dedup();
    }
    let next = Proposal {
        state: to,
        approvals,
        updated_ms: now,
        extras: with_prev(p.extras.clone(), head),
        ..p.clone()
    };
    commit_version(
        repo,
        id,
        head,
        Object::Proposal(next),
        actor,
        &format!("proposal → {}", to.name()),
    )
}

#[derive(Clone, Debug, Serialize)]
pub struct ProposalIntegrateReport {
    pub proposal: ObjectId,
    pub proposal_version: ObjectId,
    pub change_version: ObjectId,
    pub integration: integrate::IntegrateOutcome,
}

/// Atomic proposal integration: the merge (if any) is computed first, then a
/// SINGLE transaction moves: workspace position ref + proposal chain +
/// change chain. Any crash before the commit point changes nothing; after
/// it, recovery completes all three. Files are checked out afterwards
/// (repair via `newgit checkout`, same contract as integrate).
pub fn proposal_integrate(
    repo: &Repo,
    proposal_id: ObjectId,
    workspace: &str,
    actor: ObjectId,
    ts: Option<i64>,
) -> Result<ProposalIntegrateReport> {
    let _lock = crate::repo::workspace::lock(repo, workspace)?;
    let (p_head, p_obj) = current(repo, proposal_id, ObjectType::Proposal)?;
    let p = p_obj.as_proposal()?;
    if p.state != ProposalState::Approved {
        return Err(Error::Invalid(format!(
            "proposal must be approved before integration (state: {})",
            p.state.name()
        )));
    }
    let (c_head, c_obj) = current(repo, p.change, ObjectType::Change)?;
    let c = c_obj.as_change()?;
    let now = ts.unwrap_or_else(txn::now_ms);
    let ws = crate::repo::workspace::info(repo, workspace)?;
    let ours = ws.head_oid;

    // Compute the integration outcome WITHOUT committing.
    enum Plan {
        UpToDate,
        FastForward,
        Merge {
            root: ObjectId,
            snapshot_oid: ObjectId,
            entries: usize,
        },
    }
    let plan = match ours {
        None => Plan::FastForward,
        Some(o) if o == c.result => Plan::UpToDate,
        Some(o) => {
            if crate::merge::base::is_ancestor(repo, c.result, o)? {
                Plan::UpToDate
            } else if crate::merge::base::is_ancestor(repo, o, c.result)? {
                Plan::FastForward
            } else {
                let base_oid = crate::merge::base::merge_base(repo, o, c.result)?;
                let empty = repo
                    .objects
                    .put(&Object::Tree(crate::object::types::Tree::empty()))?;
                let base_root = match base_oid {
                    Some(b) => repo.objects.get(&b)?.as_snapshot()?.root,
                    None => empty,
                };
                let our_root = repo.objects.get(&o)?.as_snapshot()?.root;
                let their_root = repo.objects.get(&c.result)?.as_snapshot()?.root;
                let m = crate::merge::merge_trees(
                    repo,
                    base_root,
                    our_root,
                    their_root,
                    &crate::merge::MergeOpts::default(),
                )?;
                if !m.clean {
                    return Err(integrate::conflict_error(&m));
                }
                let snap = crate::object::types::Snapshot {
                    parents: [o, c.result].into_iter().collect(),
                    root: m.root,
                    author: actor,
                    timestamp_ms: now,
                    tz_offset_min: 0,
                    message: format!("integrate proposal {} ({})", proposal_id.short(), p.title),
                    workspace: Some(workspace.to_string()),
                    change: Some(p.change),
                    goal: c.goal,
                    extras: {
                        let mut x = BTreeMap::new();
                        x.insert("op".into(), "proposal-integrate".into());
                        x.insert("proposal".into(), proposal_id.to_hex());
                        x.insert("merge_ours".into(), o.to_hex());
                        x.insert("merge_theirs".into(), c.result.to_hex());
                        x
                    },
                };
                snap.validate()?;
                let snapshot_oid = repo.objects.put(&Object::Snapshot(snap))?;
                Plan::Merge {
                    root: m.root,
                    snapshot_oid,
                    entries: m.entries,
                }
            }
        }
    };

    // Build the single atomic transaction.
    let mut ops: Vec<TxnOp> = Vec::new();
    let new_position = match &plan {
        Plan::UpToDate => ours,
        Plan::FastForward => Some(c.result),
        Plan::Merge { snapshot_oid, .. } => Some(*snapshot_oid),
    };
    if new_position != ours {
        ops.push(TxnOp::Ref {
            name: ws.ref_name.clone(),
            cas: Cas::Exactly(ours),
            new: new_position,
            log: RefLogEntry {
                actor: Some(actor),
                ts_ms: now,
                message: format!("proposal-integrate {}", proposal_id.short()),
            },
        });
    }
    // proposal → integrated
    let mut p_extras = with_prev(p.extras.clone(), p_head);
    if let Some(np) = new_position {
        if ours != Some(c.result) || matches!(plan, Plan::Merge { .. }) {
            p_extras.insert("integration_snapshot".into(), np.to_hex());
        }
    }
    let p_next = Proposal {
        state: ProposalState::Integrated,
        updated_ms: now,
        extras: p_extras,
        ..p.clone()
    };
    p_next.validate()?;
    let p_next_oid = repo.objects.put(&Object::Proposal(p_next))?;
    ops.push(TxnOp::Ref {
        name: chain_ref(proposal_id),
        cas: Cas::Exactly(Some(p_head)),
        new: Some(p_next_oid),
        log: RefLogEntry {
            actor: Some(actor),
            ts_ms: now,
            message: "proposal → integrated".into(),
        },
    });
    // change → integrated
    let c_next = Change {
        status: ChangeStatus::Integrated,
        updated_ms: now,
        extras: with_prev(c.extras.clone(), c_head),
        ..c.clone()
    };
    c_next.validate()?;
    let c_next_oid = repo.objects.put(&Object::Change(c_next))?;
    ops.push(TxnOp::Ref {
        name: chain_ref(p.change),
        cas: Cas::Exactly(Some(c_head)),
        new: Some(c_next_oid),
        log: RefLogEntry {
            actor: Some(actor),
            ts_ms: now,
            message: "change → integrated".into(),
        },
    });

    crate::util::fault::fault("proposal:before_txn")?;
    txn::execute(repo.ng(), ops, repo.limits())?;
    let _ = crate::util::fault::fault_action("proposal:after_txn");
    if new_position != ours {
        integrate::checkout_position(repo, workspace)?;
    }
    let outcome = match plan {
        Plan::UpToDate => integrate::IntegrateOutcome::UpToDate {
            position: ours.unwrap_or(c.result),
        },
        Plan::FastForward => integrate::IntegrateOutcome::FastForward {
            from: ours,
            to: c.result,
        },
        Plan::Merge {
            root,
            snapshot_oid,
            entries,
        } => integrate::IntegrateOutcome::Merged {
            oid: snapshot_oid,
            root,
            parents: vec![ours.unwrap(), c.result],
            entries,
            renames: Vec::new(),
        },
    };
    Ok(ProposalIntegrateReport {
        proposal: proposal_id,
        proposal_version: p_next_oid,
        change_version: c_next_oid,
        integration: outcome,
    })
}

/// Fetch a typed entity for display helpers.
pub fn goal(repo: &Repo, id: ObjectId) -> Result<(ObjectId, Goal)> {
    let (h, o) = current(repo, id, ObjectType::Goal)?;
    Ok((h, o.as_goal()?.clone()))
}
pub fn change(repo: &Repo, id: ObjectId) -> Result<(ObjectId, Change)> {
    let (h, o) = current(repo, id, ObjectType::Change)?;
    Ok((h, o.as_change()?.clone()))
}
pub fn proposal(repo: &Repo, id: ObjectId) -> Result<(ObjectId, Proposal)> {
    let (h, o) = current(repo, id, ObjectType::Proposal)?;
    Ok((h, o.as_proposal()?.clone()))
}

/// Snapshot-history filter: all snapshots mentioning a goal, walked from
/// `from` (None ⇒ HEAD).
pub fn history_for_goal(
    repo: &Repo,
    from: Option<ObjectId>,
    goal_id: ObjectId,
    limit: usize,
) -> Result<Vec<crate::ops::history::HistoryEntry>> {
    let all = crate::ops::history::history(repo, from, 0)?;
    Ok(all
        .into_iter()
        .filter(|e| e.snapshot.goal == Some(goal_id))
        .take(limit)
        .collect())
}

/// Keep the Actor type referenced for CLI helpers building producers.
pub fn ensure_actor(repo: &Repo, a: &Actor) -> Result<ObjectId> {
    repo.register_actor(a)
}

/// IntegrateRequest re-export convenience for the CLI.
pub fn integrate_req(
    ws: &str,
    other: ObjectId,
    actor: ObjectId,
    ts: Option<i64>,
) -> IntegrateRequest {
    IntegrateRequest {
        workspace: ws.to_string(),
        other,
        message: None,
        author: actor,
        timestamp_ms: ts,
        merge_opts: Default::default(),
    }
}
