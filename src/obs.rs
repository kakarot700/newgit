//! Structured diagnostics (observability).
//!
//! When `--debug` or `NEWGIT_LOG=json` is active, events are written to
//! stderr as JSON lines: timestamp, per-process operation id, event name,
//! and explicit fields. RULE: fields must never contain file contents,
//! object payloads, tokens, or other secrets — ids, names, sizes, and
//! durations only (SECURITY_MODEL §6).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use serde_json::{json, Value};

static ENABLED: AtomicBool = AtomicBool::new(false);

static OP_ID: LazyLock<String> = LazyLock::new(|| {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{:x}", std::process::id(), nanos)
});

pub fn init(debug: bool) {
    let env = std::env::var("NEWGIT_LOG").unwrap_or_default();
    ENABLED.store(debug || env == "json", Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn event(name: &str, fields: &[(&str, Value)]) {
    if !enabled() {
        return;
    }
    let mut obj = serde_json::Map::new();
    obj.insert(
        "ts".into(),
        json!(crate::util::timefmt::iso8601_utc(crate::repo::txn::now_ms())),
    );
    obj.insert("op_id".into(), json!(OP_ID.as_str()));
    obj.insert("version".into(), json!(crate::VERSION));
    obj.insert("event".into(), json!(name));
    for (k, v) in fields {
        obj.insert((*k).to_string(), v.clone());
    }
    eprintln!("{}", Value::Object(obj));
}

pub fn error_event(e: &crate::error::Error) {
    event(
        "error",
        &[
            ("category", json!(e.category())),
            // message may quote small user inputs (names); never contents
            ("message", json!(e.to_string())),
        ],
    );
}

/// Timing span: logs `span_end` with duration on drop.
#[derive(Debug)]
pub struct Span {
    name: &'static str,
    start: Instant,
}

pub fn span(name: &'static str) -> Span {
    event("span_start", &[("span", json!(name))]);
    Span {
        name,
        start: Instant::now(),
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        event(
            "span_end",
            &[
                ("span", json!(self.name)),
                (
                    "duration_ms",
                    json!(self.start.elapsed().as_millis() as u64),
                ),
            ],
        );
    }
}
