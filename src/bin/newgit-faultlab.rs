//! newgit-faultlab — crash-test harness binary (NOT part of the user CLI).
//!
//! Performs single repository operations so tests can run them in a child
//! process with `NEWGIT_FAULTS`/`NEWGIT_FAULT_MODE` set and kill the process
//! at exact points, then verify recovery invariants.
//!
//! Usage:
//!   newgit-faultlab put-blob <root> <text>
//!   newgit-faultlab txn-set <root> <ref> <oid-hex|ZERO> [--expect <oid|ZERO|absent>] [--msg <m>]
//!   newgit-faultlab txn-two <root> <ref1> <oid1> <ref2> <oid2>
//!   newgit-faultlab recover <root>
//!
//! Prints one line on success (`OK <details…>`); exits with the library's
//! stable error exit codes on failure.

use newgit::error::Result;
use newgit::object::types::Object;
use newgit::object::ObjectId;
use newgit::repo::txn::{Cas, RefLogEntry, TxnOp};
use newgit::repo::Repo;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("faultlab: {e}");
            e.exit_code()
        }
    };
    std::process::exit(code);
}

fn parse_oid(s: &str) -> Result<Option<ObjectId>> {
    if s == "ZERO" {
        Ok(None)
    } else {
        Ok(Some(ObjectId::from_hex(s)?))
    }
}

fn run(args: &[String]) -> Result<()> {
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "put-blob" => {
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let oid = repo
                .objects
                .put(&Object::Blob(args[3].clone().into_bytes()))?;
            println!("OK {oid}");
            Ok(())
        }
        "txn-set" => {
            let root = std::path::Path::new(&args[2]);
            let name = args[3].clone();
            let new = parse_oid(&args[4])?;
            let mut cas = Cas::Any;
            let mut msg = "faultlab".to_string();
            let mut i = 5;
            while i < args.len() {
                match args[i].as_str() {
                    "--expect" => {
                        i += 1;
                        cas = match args[i].as_str() {
                            "absent" => Cas::Exactly(None),
                            other => Cas::Exactly(parse_oid(other)?),
                        };
                    }
                    "--msg" => {
                        i += 1;
                        msg = args[i].clone();
                    }
                    _ => {}
                }
                i += 1;
            }
            let repo = Repo::open(root)?;
            repo.refs
                .update(&name, cas, new, RefLogEntry::system(msg))?;
            println!("OK set {name}");
            Ok(())
        }
        "txn-two" => {
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let ops = vec![
                TxnOp::Ref {
                    name: args[3].clone(),
                    cas: Cas::Any,
                    new: parse_oid(&args[4])?,
                    log: RefLogEntry::system("faultlab two-phase 1"),
                },
                TxnOp::Ref {
                    name: args[5].clone(),
                    cas: Cas::Any,
                    new: parse_oid(&args[6])?,
                    log: RefLogEntry::system("faultlab two-phase 2"),
                },
            ];
            newgit::repo::txn::execute(repo.ng(), ops, repo.limits())?;
            println!("OK two");
            Ok(())
        }
        "recover" => {
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let (rep, swept) = repo.recover()?;
            println!(
                "OK redone={:?} quarantined={:?} cleaned={} swept={swept}",
                rep.redone, rep.quarantined, rep.cleaned
            );
            Ok(())
        }
        "snapshot" => {
            // snapshot <root> <message> [workspace]
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let author = repo.default_actor()?;
            let req = newgit::ops::snapshot::SnapshotRequest {
                workspace: args.get(4).cloned().unwrap_or_else(|| "main".into()),
                message: args[3].clone(),
                author,
                timestamp_ms: Some(1_700_000_000_000),
                tz_offset_min: 0,
                goal: None,
                change: None,
                extras: Default::default(),
            };
            let out = newgit::ops::snapshot::snapshot(&repo, &req)?;
            println!(
                "OK {} files={} hashed={} reused={}",
                out.oid, out.entries, out.hashed, out.reused
            );
            Ok(())
        }
        "integrate" => {
            // integrate <root> <workspace> <other-oid-hex>
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let author = repo.default_actor()?;
            let other = ObjectId::from_hex(&args[4])?;
            let req = newgit::ops::integrate::IntegrateRequest {
                workspace: args[3].clone(),
                other,
                message: None,
                author,
                timestamp_ms: Some(1_700_000_001_000),
                merge_opts: Default::default(),
            };
            let out = newgit::ops::integrate::integrate(&repo, &req)?;
            println!("OK {:?}", out);
            Ok(())
        }
        "proposal-integrate" => {
            // proposal-integrate <root> <workspace> <proposal-oid-hex>
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let actor = repo.default_actor()?;
            let pid = ObjectId::from_hex(&args[4])?;
            let rep = newgit::ops::workflow::proposal_integrate(
                &repo,
                pid,
                &args[3],
                actor,
                Some(1_700_000_002_000),
            )?;
            println!("OK {} {}", rep.proposal_version, rep.change_version);
            Ok(())
        }
        "workspace-create" => {
            // workspace-create <root> <name>
            let root = std::path::Path::new(&args[2]);
            let repo = Repo::open(root)?;
            let actor = repo.default_actor()?;
            let info = newgit::repo::workspace::create(&repo, &args[3], None, actor)?;
            println!("OK {}", info.name);
            Ok(())
        }
        other => Err(newgit::error::Error::Invalid(format!(
            "faultlab: unknown command {other:?}"
        ))),
    }
}
