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
                "OK redone={:?} quarantined={:?} swept={swept}",
                rep.redone, rep.quarantined
            );
            Ok(())
        }
        other => {
            eprintln!("faultlab: unknown command {other:?}");
            Ok(())
        }
    }
}
