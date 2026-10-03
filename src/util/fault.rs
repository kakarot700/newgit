//! Fault injection for crash-safety tests.
//!
//! A named fault point fires when its name appears in the comma-separated
//! `NEWGIT_FAULTS` environment variable. The action depends on
//! `NEWGIT_FAULT_MODE`:
//! * `abort` (default): the process aborts immediately (simulates power loss
//!   / SIGKILL at the worst possible moment).
//! * `error`: the point returns an Err (simulates I/O failure without death).
//!
//! Fault points are documented in docs/TESTING.md and only fire when the
//! environment variable is explicitly set — production runs never set it.

/// Counts how many times each named point has fired in this process, so tests
/// can fire a fault only on the Nth occurrence (`name#N` syntax).
static HITS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[derive(Debug)]
pub enum FaultAction {
    None,
    Abort,
    Error,
}

/// Check whether fault point `name` should fire. If it fires with mode=abort
/// the process is killed and this never returns.
pub fn fault_action(name: &str) -> FaultAction {
    let spec = match std::env::var("NEWGIT_FAULTS") {
        Ok(v) if !v.is_empty() => v,
        _ => return FaultAction::None,
    };
    // Spec entries are either:
    // * an exact point name ("txn:apply#1" matches the point of that name), or
    // * "pointname#N" meaning: fire on the Nth (1-based) hit of "pointname".
    let mut want_nth: Option<u64> = None;
    let mut matched = false;
    for entry in spec.split(',') {
        if entry == name {
            matched = true;
            want_nth = None;
            break;
        }
        if let Some((ename, nth)) = entry.split_once('#') {
            if ename == name {
                if let Ok(n) = nth.parse::<u64>() {
                    matched = true;
                    want_nth = Some(n);
                    break;
                }
            }
        }
    }
    if !matched {
        return FaultAction::None;
    }
    let count = {
        let mut hits = HITS.lock().unwrap_or_else(|e| e.into_inner());
        let c = hits.entry(name.to_string()).or_insert(0);
        *c += 1;
        *c
    };
    if let Some(nth) = want_nth {
        // fire on the Nth (1-based) occurrence
        if count != nth {
            return FaultAction::None;
        }
    }
    match std::env::var("NEWGIT_FAULT_MODE").as_deref() {
        Ok("error") => FaultAction::Error,
        _ => {
            // Flush stdio so test output is not lost, then die hard.
            eprintln!("newgit: fault point '{name}' firing (abort, hit #{count})");
            let _ = std::io::Write::flush(&mut std::io::stderr());
            std::process::abort();
        }
    }
}

/// Convenience: fire point; return Err(msg) in error mode, abort otherwise,
/// or Ok(()) when the point is inactive.
pub fn fault(name: &str) -> crate::error::Result<()> {
    match fault_action(name) {
        FaultAction::None => Ok(()),
        FaultAction::Error => Err(crate::error::Error::Bug(format!(
            "injected fault at '{name}'"
        ))),
        FaultAction::Abort => unreachable!(),
    }
}

/// Total number of faults fired in this process (for test assertions).
pub fn total_hits() -> u64 {
    HITS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .sum::<u64>()
}
