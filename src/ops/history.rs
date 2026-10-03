//! History traversal: deterministic newest-first topological walk.
//!
//! Order: a priority queue keyed by (timestamp_ms DESC, oid DESC) guarantees
//! the same output for the same graph regardless of insertion timing. Ties
//! break on oid so output is fully deterministic.

use std::collections::{BinaryHeap, HashSet};

use crate::error::{Error, Result};
use crate::object::types::Snapshot;
use crate::object::ObjectId;
use crate::repo::Repo;

#[derive(Clone, Debug, serde::Serialize)]
pub struct HistoryEntry {
    pub oid: ObjectId,
    pub snapshot: Snapshot,
}

#[derive(PartialEq, Eq)]
struct Item(ObjectId, Snapshot);

impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Item {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // max-heap on (timestamp, oid)
        (self.1.timestamp_ms, self.0).cmp(&(other.1.timestamp_ms, other.0))
    }
}

/// Walk history from `from` (defaults to HEAD). `limit = 0` means unlimited
/// (capped internally at 10M to avoid runaway traversal on hostile graphs).
pub fn history(repo: &Repo, from: Option<ObjectId>, limit: usize) -> Result<Vec<HistoryEntry>> {
    let start = match from {
        Some(o) => o,
        None => match repo.resolve_head()? {
            Some(o) => o,
            None => return Ok(Vec::new()), // unborn
        },
    };
    let cap = if limit == 0 { 10_000_000 } else { limit };
    let mut heap = BinaryHeap::new();
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut out = Vec::new();

    let obj = repo.objects.get(&start)?;
    let snap = obj
        .as_snapshot()
        .map_err(|e| Error::Invalid(format!("history start is not a snapshot: {e}")))?
        .clone();
    heap.push(Item(start, snap));
    visited.insert(start);

    while let Some(Item(oid, snap)) = heap.pop() {
        out.push(HistoryEntry {
            oid,
            snapshot: snap.clone(),
        });
        if out.len() >= cap {
            break;
        }
        for p in &snap.parents {
            if visited.insert(*p) {
                let pobj = repo.objects.get(p)?;
                let psnap = pobj
                    .as_snapshot()
                    .map_err(|e| Error::Corrupt {
                        oid: *p,
                        reason: format!("parent is not a snapshot: {e}"),
                    })?
                    .clone();
                heap.push(Item(*p, psnap));
            }
        }
    }
    Ok(out)
}
