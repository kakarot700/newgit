//! Workflow entity tests: goals, changes, evidence (incl. runner-recorded),
//! evaluations, proposals, atomic proposal integration, chain CAS races,
//! and crash safety.

mod common;

use std::collections::BTreeMap;

use common::*;
use newgit::error::Error;
use newgit::object::types::{ChangeStatus, GoalStatus, ObjectType, ProposalState, Verdict};
use newgit::object::ObjectId;
use newgit::ops::snapshot::{snapshot, SnapshotRequest};
use newgit::ops::status::status;
use newgit::ops::workflow::*;
use newgit::repo::workspace;

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

fn req(repo: &newgit::repo::Repo, ws: &str, msg: &str, ts: i64) -> SnapshotRequest {
    let author = repo.default_actor().unwrap();
    SnapshotRequest {
        workspace: ws.into(),
        message: msg.into(),
        author,
        timestamp_ms: Some(ts),
        tz_offset_min: 0,
        goal: None,
        change: None,
        extras: BTreeMap::new(),
    }
}

#[test]
fn goal_lifecycle_and_transitions() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    let g = goal_create(&repo, "Ship v1", "the whole thing", actor, Some(1000)).unwrap();
    let (head, g0) = goal(&repo, g).unwrap();
    assert_eq!(head, g);
    assert_eq!(g0.status, GoalStatus::Open);
    assert_eq!(g0.title, "Ship v1");

    // legal: open → in_progress → achieved
    let v = goal_set_status(&repo, g, GoalStatus::InProgress, actor, Some(1001)).unwrap();
    assert_ne!(v, g);
    assert_eq!(goal(&repo, g).unwrap().1.status, GoalStatus::InProgress);
    // illegal: in_progress → open is legal (reopen) but achieved → open is not
    let _v2 = goal_set_status(&repo, g, GoalStatus::Achieved, actor, Some(1002)).unwrap();
    let r = goal_set_status(&repo, g, GoalStatus::Open, actor, Some(1003));
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    // reopen path achieved → in_progress is legal
    goal_set_status(&repo, g, GoalStatus::InProgress, actor, Some(1004)).unwrap();
    // same-status no-op returns head
    let head_now = goal(&repo, g).unwrap().0;
    let same = goal_set_status(&repo, g, GoalStatus::InProgress, actor, Some(1005)).unwrap();
    assert_eq!(same, head_now);
    // timestamp must not go backwards
    let r = goal_set_status(&repo, g, GoalStatus::Achieved, actor, Some(1));
    assert!(matches!(r, Err(Error::Invalid(_))));

    // chain audit trail: prev links walk from head back to the root version
    let (head, _) = goal(&repo, g).unwrap();
    let mut cur = head;
    let mut versions = 1;
    while cur != g {
        let obj = repo.objects.get(&cur).unwrap();
        let gobj = obj.as_goal().unwrap();
        let prev = gobj
            .extras
            .get("prev")
            .unwrap_or_else(|| panic!("non-root version {cur} must have prev"));
        cur = ObjectId::from_hex(prev).unwrap();
        versions += 1;
        assert!(versions < 20, "cycle in prev chain");
    }
    assert!(versions >= 4, "create + 3 status versions, got {versions}");
    // list
    let list = list_entities(&repo, ObjectType::Goal).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, g);
}

#[test]
fn change_lifecycle_honesty_gates() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "main", "s1", 2000)).unwrap();
    let g = goal_create(&repo, "goal", "", actor, Some(2000)).unwrap();
    write(repo.root(), "f.txt", b"2\n");
    let s2 = snapshot(&repo, &req(&repo, "main", "s2", 2001)).unwrap();

    // validation: base must be a snapshot; base==result rejected
    let r = change_create(
        &repo,
        &ChangeInput {
            base: g,
            result: s2.oid,
            author: actor,
            goal: None,
            title: "t".into(),
            description: "".into(),
            ts: Some(2002),
        },
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}"); // g is a goal, not snapshot
    let r = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s1.oid,
            author: actor,
            goal: None,
            title: "t".into(),
            description: "".into(),
            ts: Some(2002),
        },
    );
    assert!(r.is_err(), "empty change rejected");

    let c = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s2.oid,
            author: actor,
            goal: Some(g),
            title: "bump".into(),
            description: "one to two".into(),
            ts: Some(2002),
        },
    )
    .unwrap();
    // honesty gate: draft → tested REQUIRES evidence
    let r = change_set_status(&repo, c, ChangeStatus::Tested, actor, Some(2003));
    match r {
        Err(Error::Invalid(msg)) => assert!(msg.contains("no evidence"), "{msg}"),
        other => panic!("{other:?}"),
    }
    // illegal skip-level transition
    let r = change_set_status(&repo, c, ChangeStatus::Proposed, actor, Some(2003));
    assert!(matches!(r, Err(Error::Invalid(_))));

    // record evidence with the runner (real command execution)
    let rep = evidence_record(
        &repo,
        actor,
        Some(c),
        "unit_test",
        repo.root(),
        &["sh".into(), "-c".into(), "echo hello-evidence".into()],
        Some(2004),
    )
    .unwrap();
    assert_eq!(rep.verdict, Verdict::Pass);
    assert_eq!(rep.exit_code, Some(0));
    assert!(!rep.signal && !rep.truncated);
    let ev_obj = repo.objects.get(&rep.evidence).unwrap();
    let ev = ev_obj.as_evidence().unwrap();
    assert!(
        ev.deterministic,
        "runner-recorded evidence is deterministic"
    );
    let out_blob = repo.objects.get(&ev.output.unwrap()).unwrap();
    assert!(out_blob.as_blob().unwrap().starts_with(b"hello-evidence"));
    assert!(ev.metrics.contains_key("duration_ms"));
    assert_eq!(ev.metrics["exit_code"], "0");
    assert_eq!(ev.command, "sh -c echo hello-evidence");

    // failing command → Fail verdict, still deterministic
    let rep_fail = evidence_record(
        &repo,
        actor,
        Some(c),
        "unit_test",
        repo.root(),
        &["sh".into(), "-c".into(), "exit 3".into()],
        Some(2005),
    )
    .unwrap();
    assert_eq!(rep_fail.verdict, Verdict::Fail);
    assert_eq!(rep_fail.exit_code, Some(3));

    // attach the PASS evidence, then tested is allowed
    change_attach_evidence(&repo, c, rep.evidence, actor, Some(2006)).unwrap();
    // attach is idempotent
    let v_again = change_attach_evidence(&repo, c, rep.evidence, actor, Some(2007)).unwrap();
    assert_eq!(change(&repo, c).unwrap().1.evidence.len(), 1);
    assert_eq!(
        v_again,
        change(&repo, c).unwrap().0,
        "idempotent attach returns head"
    );
    change_set_status(&repo, c, ChangeStatus::Tested, actor, Some(2008)).unwrap();
    assert_eq!(change(&repo, c).unwrap().1.status, ChangeStatus::Tested);

    // opinion evidence must NOT claim deterministic
    let op = evidence_add(
        &repo,
        &EvidenceInput {
            producer: actor,
            target: Some(c),
            kind: "review".into(),
            verdict: Verdict::Pass,
            deterministic: false,
            tool: "human".into(),
            tool_version: String::new(),
            command: String::new(),
            output: None,
            metrics: BTreeMap::new(),
            ts: Some(2009),
        },
    )
    .unwrap();
    let oev = repo.objects.get(&op).unwrap();
    assert!(!oev.as_evidence().unwrap().deterministic);

    // evaluation aggregation: pass evidence + pass opinion + FAIL evidence
    change_attach_evidence(&repo, c, rep_fail.evidence, actor, Some(2010)).unwrap();
    change_attach_evidence(&repo, c, op, actor, Some(2010)).unwrap();
    let agg = evaluation_from_evidence(&repo, c, actor, Some(2011)).unwrap();
    let aobj = repo.objects.get(&agg).unwrap();
    let aev = aobj.as_evaluation().unwrap();
    assert_eq!(aev.verdict, Verdict::Fail, "any fail ⇒ fail");
    assert!(!aev.ai_generated);
    assert_eq!(aev.target, c);
    // dimension per kind with worst verdict
    let dim = aev
        .dimensions
        .iter()
        .find(|(n, _, _)| n == "unit_test")
        .unwrap();
    assert_eq!(dim.1, Verdict::Fail);
    let dim_r = aev
        .dimensions
        .iter()
        .find(|(n, _, _)| n == "review")
        .unwrap();
    assert_eq!(dim_r.1, Verdict::Pass);
    assert!(dim_r.2.contains("opinion"), "opinion is labeled as such");
}

#[test]
fn proposal_flow_and_atomic_integration() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    write(repo.root(), "f.txt", b"base\n");
    let s1 = snapshot(&repo, &req(&repo, "main", "base", 3000)).unwrap();
    let wa = workspace::create(&repo, "agentw", Some(s1.oid), actor).unwrap();
    write(&wa.dir, "feature.txt", b"new feature\n");
    write(&wa.dir, "f.txt", b"base\nmore\n");
    let s2 = snapshot(&repo, &req(&repo, "agentw", "feature", 3001)).unwrap();
    // main moved on (disjoint file)
    write(repo.root(), "main.txt", b"main work\n");
    let s3 = snapshot(&repo, &req(&repo, "main", "main work", 3002)).unwrap();

    let c = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s2.oid,
            author: actor,
            goal: None,
            title: "feature".into(),
            description: String::new(),
            ts: Some(3003),
        },
    )
    .unwrap();
    let ev = evidence_record(
        &repo,
        actor,
        Some(c),
        "build",
        &wa.dir,
        &["sh".into(), "-c".into(), "test -f feature.txt".into()],
        Some(3004),
    )
    .unwrap();
    change_attach_evidence(&repo, c, ev.evidence, actor, Some(3005)).unwrap();
    change_set_status(&repo, c, ChangeStatus::Tested, actor, Some(3006)).unwrap();

    // proposal requires tested/proposed change ✓
    let p = proposal_create(
        &repo,
        &ProposalInput {
            change: c,
            title: "Add feature".into(),
            rationale: "because".into(),
            author: actor,
            base: s3.oid, // integrate onto CURRENT main
            evidence: vec![ev.evidence],
            depends_on: vec![],
            ts: Some(3007),
        },
    )
    .unwrap();
    // state machine: cannot jump to integrated
    let r = proposal_transition(&repo, p, ProposalState::Integrated, actor, Some(3008));
    assert!(r.is_err());
    // cannot integrate while open
    let r = proposal_integrate(&repo, p, "main", actor, Some(3009));
    assert!(r.is_err());
    // approve
    proposal_transition(&repo, p, ProposalState::Approved, actor, Some(3010)).unwrap();
    let (approved_head, prop) = proposal(&repo, p).unwrap();
    assert_eq!(prop.state, ProposalState::Approved);
    assert_eq!(prop.approvals.len(), 1);
    assert_eq!(prop.approvals[0].0, actor);

    // ATOMIC integration onto main (position s3, change result s2 → merge)
    let _rep = proposal_integrate(&repo, p, "main", actor, Some(3011)).unwrap();
    // 1) position moved to a merge snapshot
    let head = repo.refs.read("refs/main").unwrap();
    assert_ne!(head, s3.oid);
    let hs = repo.objects.get(&head).unwrap();
    let hsnap = hs.as_snapshot().unwrap();
    assert_eq!(hsnap.parents.len(), 2);
    assert!(hsnap.parents.contains(&s3.oid) && hsnap.parents.contains(&s2.oid));
    assert_eq!(hsnap.extras["proposal"], p.to_hex());
    // 2) proposal chain → integrated, prev points at the approved version
    let (_, prop2) = proposal(&repo, p).unwrap();
    assert_eq!(prop2.state, ProposalState::Integrated);
    assert_eq!(prop2.extras["prev"], approved_head.to_hex());
    // 3) change chain → integrated
    assert_eq!(change(&repo, c).unwrap().1.status, ChangeStatus::Integrated);
    // 4) files materialized: feature.txt AND main.txt both present
    assert!(repo.root().join("feature.txt").exists());
    assert!(repo.root().join("main.txt").exists());
    assert_eq!(
        std::fs::read(repo.root().join("f.txt")).unwrap(),
        b"base\nmore\n"
    );
    assert!(status(&repo, "main", 10).unwrap().clean);
    // re-integrate is a no-op error (already integrated state machine)
    let r = proposal_integrate(&repo, p, "main", actor, Some(3012));
    assert!(r.is_err());
}

#[test]
fn proposal_integrate_conflict_writes_nothing() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    write(repo.root(), "f.txt", b"line\n");
    let s1 = snapshot(&repo, &req(&repo, "main", "base", 4000)).unwrap();
    let wa = workspace::create(&repo, "cw", Some(s1.oid), actor).unwrap();
    write(&wa.dir, "f.txt", b"THEIRS\n");
    let s2 = snapshot(&repo, &req(&repo, "cw", "their edit", 4001)).unwrap();
    write(repo.root(), "f.txt", b"OURS\n");
    let s3 = snapshot(&repo, &req(&repo, "main", "our edit", 4002)).unwrap();
    let c = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s2.oid,
            author: actor,
            goal: None,
            title: "t".into(),
            description: "".into(),
            ts: Some(4003),
        },
    )
    .unwrap();
    let ev = evidence_add(
        &repo,
        &EvidenceInput {
            producer: actor,
            target: Some(c),
            kind: "review".into(),
            verdict: Verdict::Pass,
            deterministic: false,
            tool: String::new(),
            tool_version: String::new(),
            command: String::new(),
            output: None,
            metrics: BTreeMap::new(),
            ts: Some(4004),
        },
    )
    .unwrap();
    change_attach_evidence(&repo, c, ev, actor, Some(4005)).unwrap();
    change_set_status(&repo, c, ChangeStatus::Tested, actor, Some(4006)).unwrap();
    let p = proposal_create(
        &repo,
        &ProposalInput {
            change: c,
            title: "t".into(),
            rationale: String::new(),
            author: actor,
            base: s3.oid,
            evidence: vec![ev],
            depends_on: vec![],
            ts: Some(4007),
        },
    )
    .unwrap();
    proposal_transition(&repo, p, ProposalState::Approved, actor, Some(4008)).unwrap();
    let r = proposal_integrate(&repo, p, "main", actor, Some(4009));
    match r {
        Err(Error::Conflict(_)) => {}
        other => panic!("expected conflict, got {other:?}"),
    }
    // nothing moved
    assert_eq!(repo.refs.read("refs/main").unwrap(), s3.oid);
    assert_eq!(proposal(&repo, p).unwrap().1.state, ProposalState::Approved);
    assert_eq!(change(&repo, c).unwrap().1.status, ChangeStatus::Tested);
    assert_eq!(std::fs::read(repo.root().join("f.txt")).unwrap(), b"OURS\n");
}

#[test]
fn chain_updates_under_concurrency_stay_linear() {
    // Two threads attempt DIFFERENT transitions from the same state behind a
    // barrier. Any interleaving is legal; the invariants that must hold:
    // * at least one succeeds,
    // * every error is cas_failed (lost the race) or invalid (transition
    //   became illegal after the other committed),
    // * the version chain from head to root is strictly linear via prev —
    //   concurrent updates never fork or lose versions.
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    let actor = repo.default_actor().unwrap();
    let g = goal_create(&repo, "race", "", actor, Some(5000)).unwrap();
    drop(repo);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let repo = newgit::repo::Repo::open(&root).unwrap();
                let actor = repo.default_actor().unwrap();
                barrier.wait();
                let to = if i == 0 {
                    GoalStatus::InProgress
                } else {
                    GoalStatus::Abandoned
                };
                goal_set_status(&repo, g, to, actor, Some(5001 + i))
                    .map(|v| (to, v))
                    .map_err(|e| e.category().to_string())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let oks = results.iter().filter(|r| r.is_ok()).count();
    assert!(oks >= 1, "{results:?}");
    for r in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            r == "cas_failed" || r == "invalid",
            "unexpected error category: {r} ({results:?})"
        );
    }
    // chain linearity from head back to the root version
    let repo = newgit::repo::Repo::open(&root).unwrap();
    let (head, _) = goal(&repo, g).unwrap();
    let mut cur = head;
    let mut versions = 1;
    let mut final_status = None;
    while cur != g {
        let obj = repo.objects.get(&cur).unwrap();
        let gobj = obj.as_goal().unwrap();
        if final_status.is_none() {
            final_status = Some(gobj.status);
        }
        let prev = gobj.extras.get("prev").expect("prev link");
        cur = ObjectId::from_hex(prev).unwrap();
        versions += 1;
        assert!(versions < 10, "chain cycle");
    }
    let root_obj = repo.objects.get(&g).unwrap();
    let final_status = final_status.unwrap_or_else(|| root_obj.as_goal().unwrap().status);
    assert!((2..=3).contains(&versions), "{versions} versions");
    // committed transitions must be exactly the oks that returned new versions
    let committed = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .filter(|(_, v)| *v != g)
        .count();
    assert_eq!(versions - 1, committed, "no lost or phantom versions");
    // the head status equals the last successful transition
    let last = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .find(|(_, v)| *v == head);
    if let Some((to, _)) = last {
        assert_eq!(final_status, *to);
    }
}

#[test]
fn snapshot_links_goal_and_change_and_history_filter() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    let g = goal_create(&repo, "linked", "", actor, Some(6000)).unwrap();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "main", "no links", 6001)).unwrap();
    write(repo.root(), "f.txt", b"2\n");
    let mut r = req(&repo, "main", "with goal", 6002);
    r.goal = Some(g);
    let s2 = snapshot(&repo, &r).unwrap();
    let c = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s2.oid,
            author: actor,
            goal: Some(g),
            title: "c".into(),
            description: "".into(),
            ts: Some(6003),
        },
    )
    .unwrap();
    write(repo.root(), "f.txt", b"3\n");
    let mut r = req(&repo, "main", "with both", 6004);
    r.goal = Some(g);
    r.change = Some(c);
    let s3 = snapshot(&repo, &r).unwrap();
    let h = history_for_goal(&repo, None, g, 10).unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].oid, s3.oid);
    assert_eq!(h[1].oid, s2.oid);
    // change list by goal
    let list = list_entities(&repo, ObjectType::Change).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].1.as_change().unwrap().goal, Some(g));
}

#[test]
fn proposal_integrate_crash_atomicity() {
    // build a full approved-proposal state, then kill the child at
    // proposal:before_txn (nothing may change) and proposal:after_txn
    // (all three refs must move together).
    for (fault, expect_moved) in [
        (Some("proposal:before_txn"), false),
        (Some("proposal:after_txn"), true),
    ] {
        let (_d, repo) = temp_repo();
        let root = repo.root().to_path_buf();
        let actor = repo.default_actor().unwrap();
        write(repo.root(), "f.txt", b"base\n");
        let s1 = snapshot(&repo, &req(&repo, "main", "base", 7000)).unwrap();
        let wa = workspace::create(&repo, "pw", Some(s1.oid), actor).unwrap();
        write(&wa.dir, "add.txt", b"added\n");
        let s2 = snapshot(&repo, &req(&repo, "pw", "add", 7001)).unwrap();
        let c = change_create(
            &repo,
            &ChangeInput {
                base: s1.oid,
                result: s2.oid,
                author: actor,
                goal: None,
                title: "t".into(),
                description: "".into(),
                ts: Some(7002),
            },
        )
        .unwrap();
        let ev = evidence_add(
            &repo,
            &EvidenceInput {
                producer: actor,
                target: Some(c),
                kind: "review".into(),
                verdict: Verdict::Pass,
                deterministic: false,
                tool: String::new(),
                tool_version: String::new(),
                command: String::new(),
                output: None,
                metrics: BTreeMap::new(),
                ts: Some(7003),
            },
        )
        .unwrap();
        change_attach_evidence(&repo, c, ev, actor, Some(7004)).unwrap();
        change_set_status(&repo, c, ChangeStatus::Tested, actor, Some(7005)).unwrap();
        let p = proposal_create(
            &repo,
            &ProposalInput {
                change: c,
                title: "t".into(),
                rationale: String::new(),
                author: actor,
                base: s1.oid,
                evidence: vec![ev],
                depends_on: vec![],
                ts: Some(7006),
            },
        )
        .unwrap();
        proposal_transition(&repo, p, ProposalState::Approved, actor, Some(7007)).unwrap();
        let pos_before = repo.refs.read("refs/main").unwrap();
        let pver_before = proposal(&repo, p).unwrap().0;
        let cver_before = change(&repo, c).unwrap().0;
        drop(repo);

        let out = run_faultlab(&root, &["proposal-integrate", "main", &p.to_hex()], fault);
        assert!(!out.status.success());
        let repo = newgit::repo::Repo::open(&root).unwrap();
        let pos = repo.refs.read("refs/main").unwrap();
        let pver = proposal(&repo, p).unwrap().0;
        let cver = change(&repo, c).unwrap().0;
        if expect_moved {
            assert_ne!(pos, pos_before, "after commit point ⇒ position moved");
            assert_ne!(pver, pver_before, "proposal chain moved");
            assert_ne!(cver, cver_before, "change chain moved");
            assert_eq!(
                proposal(&repo, p).unwrap().1.state,
                ProposalState::Integrated
            );
            assert_eq!(change(&repo, c).unwrap().1.status, ChangeStatus::Integrated);
            // files may be stale (checkout is post-commit); repair path works
            let _ = newgit::ops::integrate::checkout_position(&repo, "main");
            assert!(status(&repo, "main", 10).unwrap().clean);
        } else {
            assert_eq!(pos, pos_before, "before commit point ⇒ nothing moved");
            assert_eq!(pver, pver_before);
            assert_eq!(cver, cver_before);
            assert_eq!(proposal(&repo, p).unwrap().1.state, ProposalState::Approved);
        }
    }
}

#[test]
fn evidence_record_limits_and_signals() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    // huge output is truncated to the cap
    let rep = evidence_record(
        &repo,
        actor,
        None,
        "stress",
        repo.root(),
        &[
            "sh".into(),
            "-c".into(),
            "yes AAAA | head -c 9000000".into(),
        ],
        Some(8000),
    )
    .unwrap();
    assert!(
        rep.truncated,
        "9MB output must truncate to the 8MiB record cap"
    );
    assert!(rep.output_bytes <= (8 << 20) + 64);
    // Unix exposes signals; Windows reports the child's exit code.
    let rep2 = evidence_record(
        &repo,
        actor,
        None,
        "stress",
        repo.root(),
        &["sh".into(), "-c".into(), "kill -9 $$".into()],
        Some(8001),
    )
    .unwrap();
    #[cfg(unix)]
    {
        assert_eq!(rep2.verdict, Verdict::Inconclusive);
        assert!(rep2.signal);
        assert_eq!(rep2.exit_code, None);
    }
    #[cfg(not(unix))]
    {
        assert_eq!(rep2.verdict, Verdict::Fail);
        assert!(!rep2.signal);
        assert!(rep2.exit_code.is_some_and(|code| code != 0));
    }
    // missing command → actionable error, not a panic
    let r = evidence_record(
        &repo,
        actor,
        None,
        "x",
        repo.root(),
        &["definitely-not-a-real-binary-xyz".into()],
        Some(8002),
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn evaluation_targets_and_ai_flag() {
    let (_d, repo) = temp_repo();
    let actor = repo.default_actor().unwrap();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "main", "s1", 9000)).unwrap();
    write(repo.root(), "f.txt", b"2\n");
    let s2 = snapshot(&repo, &req(&repo, "main", "s2", 9001)).unwrap();
    let c = change_create(
        &repo,
        &ChangeInput {
            base: s1.oid,
            result: s2.oid,
            author: actor,
            goal: None,
            title: "t".into(),
            description: "".into(),
            ts: Some(9002),
        },
    )
    .unwrap();
    // ai opinion evaluation
    let ai = evaluation_add(
        &repo,
        c,
        actor,
        true,
        Verdict::Pass,
        vec![("style".into(), Verdict::Pass, "looks nice".into())],
        Some(9003),
    )
    .unwrap();
    let aobj = repo.objects.get(&ai).unwrap();
    assert!(aobj.as_evaluation().unwrap().ai_generated);
    // target type enforced (an actor object is not a valid target)
    let r = evaluation_add(
        &repo,
        actor,
        actor,
        false,
        Verdict::Pass,
        vec![],
        Some(9004),
    );
    assert!(r.is_err());
    // dimensions sorted + dedup invariant enforced by validate
    let r = evaluation_add(
        &repo,
        c,
        actor,
        false,
        Verdict::Pass,
        vec![
            ("b".into(), Verdict::Pass, String::new()),
            ("a".into(), Verdict::Pass, String::new()),
        ],
        Some(9005),
    );
    assert!(r.is_ok()); // sorted internally before validate
}
