//! Test-only wall-clock stage collection for the manual smart-HTTP benchmark.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct StageSample {
    pub(crate) name: String,
    pub(crate) elapsed: Duration,
}

static SAMPLES: OnceLock<Mutex<Vec<StageSample>>> = OnceLock::new();

pub(crate) fn record(name: impl Into<String>, elapsed: Duration) {
    SAMPLES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push(StageSample {
            name: name.into(),
            elapsed,
        });
}

pub(crate) fn clear() {
    SAMPLES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .clear();
}

pub(crate) fn take() -> Vec<StageSample> {
    std::mem::take(
        &mut *SAMPLES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap(),
    )
}
