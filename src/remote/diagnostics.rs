//! Opt-in smart-HTTP diagnostics for reproducing platform-specific test failures.
//!
//! Enable the `smart-http-diagnostics` Cargo feature and set
//! `NEWGIT_SMART_HTTP_DIAGNOSTICS=1`. Events contain timings, statuses, sizes,
//! canonical operation labels, and test correlation IDs only; never request
//! bodies, authorization headers, or arbitrary error strings.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Context {
    request_id: u64,
    started: Instant,
    client_trace_id: Option<String>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub(crate) struct RequestScope {
    previous: Option<Context>,
}

pub(crate) fn begin(
    peer: &str,
    configured_idle_timeout_ms: u64,
    configured_deadline_ms: u64,
    initial_deadline_remaining_ms: u64,
) -> RequestScope {
    let now = Instant::now();
    let context = Context {
        request_id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        started: now,
        client_trace_id: None,
    };
    let previous = CONTEXT.with(|slot| slot.replace(Some(context)));
    event(
        "request_start",
        &[
            ("peer", json!(peer)),
            (
                "configured_socket_idle_timeout_ms",
                json!(configured_idle_timeout_ms),
            ),
            (
                "configured_receive_deadline_ms",
                json!(configured_deadline_ms),
            ),
            (
                "initial_deadline_remaining_ms",
                json!(initial_deadline_remaining_ms),
            ),
        ],
    );
    RequestScope { previous }
}

impl Drop for RequestScope {
    fn drop(&mut self) {
        event("request_end", &[]);
        CONTEXT.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(crate) fn set_client_trace_id(value: Option<&str>) {
    let safe = value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .map(str::to_owned);
    CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow_mut().as_mut() {
            context.client_trace_id = safe;
        }
    });
}

pub(crate) fn event(name: &str, fields: &[(&str, Value)]) {
    if std::env::var_os("NEWGIT_SMART_HTTP_DIAGNOSTICS").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    let mut object = serde_json::Map::new();
    object.insert("ts_unix_ms".into(), json!(timestamp_ms));
    object.insert("event".into(), json!(name));
    CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow().as_ref() {
            object.insert("request_id".into(), json!(context.request_id));
            object.insert(
                "elapsed_ms".into(),
                json!(context.started.elapsed().as_millis() as u64),
            );
            if let Some(client_trace_id) = &context.client_trace_id {
                object.insert("client_trace_id".into(), json!(client_trace_id));
            }
        }
    });
    for (key, value) in fields {
        object.insert((*key).to_string(), value.clone());
    }
    eprintln!("{}", Value::Object(object));
}
