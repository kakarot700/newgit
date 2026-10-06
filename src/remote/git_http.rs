//! Read-only Git smart-HTTP adapter (upload-pack only).
//!
//! The adapter materializes a private, short-lived Git repository from the
//! current NewGit refs and object graph, then delegates Git's pkt-line, pack,
//! and v0/v1/v2 protocol behavior to the installed `git upload-pack`.
//! NewGit remains the source of truth; this module does not store Git objects
//! or mutate NewGit refs.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::repo::Repo;

const GIT_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_PACK_SERVICE_ANNOUNCEMENT: &[u8] = b"001e# service=git-upload-pack\n0000";

/// Build the smart-HTTP `info/refs` payload using Git's own upload-pack
/// implementation. `git_protocol` is the validated HTTP `Git-Protocol` value.
pub fn advertise(
    repo: &Repo,
    git_protocol: Option<&str>,
    max_response_bytes: u64,
) -> Result<Vec<u8>> {
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_operation_start",
        &[
            ("operation", serde_json::json!("upload_pack_advertise")),
            (
                "configured_git_deadline_ms",
                serde_json::json!(GIT_OPERATION_TIMEOUT.as_millis() as u64),
            ),
        ],
    );
    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let view = TempGitView::from_newgit(repo, deadline)?;
    let v2 = git_protocol == Some("version=2");
    let prefix_len = if v2 {
        0
    } else {
        UPLOAD_PACK_SERVICE_ANNOUNCEMENT.len()
    };
    if prefix_len as u64 > max_response_bytes {
        return Err(Error::Limit(format!(
            "Git upload-pack advertisement exceeds the configured {} byte limit",
            max_response_bytes
        )));
    }
    #[cfg(test)]
    let command_started = Instant::now();
    let payload = run_upload_pack(
        &view,
        &["--timeout=30", "--http-backend-info-refs"],
        git_protocol,
        None,
        max_response_bytes - prefix_len as u64,
        deadline,
    )?;
    #[cfg(test)]
    crate::remote::bench_timing::record("git_http.advertise_command", command_started.elapsed());
    if v2 {
        Ok(payload)
    } else {
        let mut response = Vec::with_capacity(prefix_len + payload.len());
        response.extend_from_slice(UPLOAD_PACK_SERVICE_ANNOUNCEMENT);
        response.extend_from_slice(&payload);
        Ok(response)
    }
}

/// Process one stateless HTTP upload-pack exchange against a fresh Git view.
pub fn upload_pack(
    repo: &Repo,
    git_protocol: Option<&str>,
    request: &[u8],
    max_response_bytes: u64,
) -> Result<Vec<u8>> {
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_operation_start",
        &[
            ("operation", serde_json::json!("upload_pack")),
            (
                "configured_git_deadline_ms",
                serde_json::json!(GIT_OPERATION_TIMEOUT.as_millis() as u64),
            ),
        ],
    );
    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let view = TempGitView::from_newgit(repo, deadline)?;
    #[cfg(test)]
    let command_started = Instant::now();
    let result = run_upload_pack(
        &view,
        &["--timeout=30", "--stateless-rpc"],
        git_protocol,
        Some(request),
        max_response_bytes,
        deadline,
    );
    #[cfg(test)]
    crate::remote::bench_timing::record("git_http.upload_pack_command", command_started.elapsed());
    result
}

/// Restrict protocol negotiation to versions implemented by the installed Git
/// upload-pack. Protocol v0 is represented by an absent header.
pub fn validate_git_protocol(value: Option<&str>) -> Result<Option<&'static str>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some("version=0") => Ok(None),
        Some("version=1") => Ok(Some("version=1")),
        Some("version=2") => Ok(Some("version=2")),
        Some(other) => Err(Error::Protocol(format!(
            "unsupported Git-Protocol value {other:?}; supported values are version=0, version=1, and version=2"
        ))),
    }
}

pub(crate) struct TempGitView {
    _directory: tempfile::TempDir,
    pub(crate) path: PathBuf,
    pub(crate) global_config: PathBuf,
    pub(crate) template_dir: PathBuf,
}

impl TempGitView {
    fn from_newgit(repo: &Repo, deadline: Instant) -> Result<Self> {
        Self::from_newgit_for_upload_pack_with_export(repo, deadline).map(|(view, _)| view)
    }

    pub(crate) fn from_newgit_with_export(
        repo: &Repo,
        deadline: Instant,
    ) -> Result<(Self, crate::gitio::export::ExportReport)> {
        Self::build(repo, deadline, true)
    }

    pub(crate) fn from_newgit_for_upload_pack_with_export(
        repo: &Repo,
        deadline: Instant,
    ) -> Result<(Self, crate::gitio::export::ExportReport)> {
        Self::build(repo, deadline, false)
    }

    fn build(
        repo: &Repo,
        deadline: Instant,
        materialize_worktree: bool,
    ) -> Result<(Self, crate::gitio::export::ExportReport)> {
        #[cfg(any(test, feature = "smart-http-diagnostics"))]
        let projection_started = Instant::now();
        #[cfg(feature = "smart-http-diagnostics")]
        crate::remote::diagnostics::event(
            "projection_start",
            &[
                (
                    "materialize_worktree",
                    serde_json::json!(materialize_worktree),
                ),
                (
                    "deadline_remaining_ms",
                    serde_json::json!(deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()),
                ),
            ],
        );
        #[cfg(test)]
        let setup_started = Instant::now();
        let directory = tempfile::Builder::new()
            .prefix("newgit-git-http-")
            .tempdir()
            .map_err(Error::from)?;
        let root = directory.path().to_path_buf();
        let view = Self {
            _directory: directory,
            path: root.join("repository"),
            global_config: root.join("empty-global-config"),
            template_dir: root.join("empty-template"),
        };
        std::fs::write(&view.global_config, b"").map_err(|e| Error::io(&view.global_config, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&view.global_config, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::io(&view.global_config, e))?;
        }
        std::fs::create_dir(&view.template_dir).map_err(|e| Error::io(&view.template_dir, e))?;
        #[cfg(test)]
        crate::remote::bench_timing::record("projection.tempdir_setup", setup_started.elapsed());
        // The existing exporter handles ref mapping, object construction,
        // merges, tree content, and symbolic HEAD.
        #[cfg(any(test, feature = "smart-http-diagnostics"))]
        let export_started = Instant::now();
        let export = {
            // Refs, HEAD, history, and their objects must all come from one
            // committed state. Recovery is performed while acquiring the
            // same exclusive lock used by transactions and GC.
            #[cfg(any(test, feature = "smart-http-diagnostics"))]
            let snapshot_wait_started = Instant::now();
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event("projection_lock_wait_start", &[]);
            let _snapshot = crate::repo::txn::SnapshotReadGuard::acquire(repo.ng(), repo.limits())?;
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event(
                "projection_lock_acquired",
                &[(
                    "lock_wait_ms",
                    serde_json::json!(snapshot_wait_started.elapsed().as_millis() as u64),
                )],
            );
            #[cfg(test)]
            crate::remote::bench_timing::record(
                "projection.snapshot_lock_wait",
                snapshot_wait_started.elapsed(),
            );
            #[cfg(any(test, feature = "smart-http-diagnostics"))]
            let snapshot_hold_started = Instant::now();
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event("projection_export_start", &[]);
            let export = if materialize_worktree {
                crate::gitio::export::export_git_isolated(
                    repo,
                    &view.path,
                    &view.global_config,
                    &view.template_dir,
                    deadline,
                )
            } else {
                crate::gitio::export::export_git_isolated_for_upload_pack(
                    repo,
                    &view.path,
                    &view.global_config,
                    &view.template_dir,
                    deadline,
                )
            };
            #[cfg(any(test, feature = "smart-http-diagnostics"))]
            let snapshot_hold_elapsed = snapshot_hold_started.elapsed();
            drop(_snapshot);
            #[cfg(feature = "smart-http-diagnostics")]
            crate::remote::diagnostics::event(
                "projection_lock_released",
                &[
                    (
                        "lock_hold_ms",
                        serde_json::json!(snapshot_hold_elapsed.as_millis() as u64),
                    ),
                    ("export_succeeded", serde_json::json!(export.is_ok())),
                ],
            );
            #[cfg(test)]
            crate::remote::bench_timing::record(
                "projection.snapshot_lock_hold",
                snapshot_hold_elapsed,
            );
            export
        };
        let export = match export {
            Ok(report) => report,
            Err(Error::Invalid(message)) if message.starts_with("nothing to export:") => {
                init_empty_view(&view, deadline)?;
                crate::gitio::export::ExportReport::default()
            }
            Err(error) => return Err(error),
        };
        #[cfg(feature = "smart-http-diagnostics")]
        crate::remote::diagnostics::event(
            "projection_export_end",
            &[
                (
                    "export_duration_ms",
                    serde_json::json!(export_started.elapsed().as_millis() as u64),
                ),
                (
                    "refs_exported",
                    serde_json::json!(export.refs_exported.len()),
                ),
                ("commits_exported", serde_json::json!(export.commits)),
                ("blobs_exported", serde_json::json!(export.blobs)),
            ],
        );
        #[cfg(test)]
        crate::remote::bench_timing::record("projection.export_total", export_started.elapsed());
        #[cfg(test)]
        crate::remote::bench_timing::record("projection.total", projection_started.elapsed());
        #[cfg(feature = "smart-http-diagnostics")]
        crate::remote::diagnostics::event(
            "projection_end",
            &[
                (
                    "projection_duration_ms",
                    serde_json::json!(projection_started.elapsed().as_millis()),
                ),
                ("success", serde_json::json!(true)),
            ],
        );
        Ok((view, export))
    }
}

fn init_empty_view(view: &TempGitView, deadline: Instant) -> Result<()> {
    let mut command = Command::new("git");
    command
        .args(["init", "--quiet", "--initial-branch=main", "--template"])
        .arg(&view.template_dir)
        .arg(&view.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &view.global_config)
        .env("GIT_CONFIG_COUNT", "0");
    isolate_git_environment(&mut command);
    #[cfg(feature = "smart-http-diagnostics")]
    let subprocess_started = Instant::now();
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_subprocess_start",
        &[("operation", serde_json::json!("empty_projection_git_init"))],
    );
    let mut child =
        crate::util::process::ManagedChild::spawn(&mut command, Some(remaining(deadline)?))
            .map_err(|e| Error::Invalid(format!("cannot initialize empty Git projection: {e}")))?;
    let (status, timed_out) = child
        .wait()
        .map_err(|e| Error::Invalid(format!("could not wait for Git init: {e}")))?;
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_subprocess_end",
        &[
            ("operation", serde_json::json!("empty_projection_git_init")),
            (
                "elapsed_ms",
                serde_json::json!(subprocess_started.elapsed().as_millis()),
            ),
            ("exit_code", serde_json::json!(status.code())),
            ("timed_out", serde_json::json!(timed_out)),
        ],
    );
    if timed_out {
        return Err(git_deadline_error());
    }
    if !status.success() {
        return Err(Error::Protocol(format!(
            "Git init for empty projection exited with status {status}"
        )));
    }
    Ok(())
}

pub(crate) fn isolate_git_environment(command: &mut Command) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_PROTOCOL",
        "GIT_TEMPLATE_DIR",
        "GIT_EXEC_PATH",
        "GIT_TRACE",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_SETUP",
    ] {
        command.env_remove(key);
    }
}

fn run_upload_pack(
    view: &TempGitView,
    args: &[&str],
    git_protocol: Option<&str>,
    request: Option<&[u8]>,
    max_response_bytes: u64,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        // These are command-scope (protected) settings, not repository config:
        // advertise partial-clone filtering, allow only blob:none, and let a
        // promisor client hydrate an object only if it is reachable from a ref.
        .arg("-c")
        .arg("uploadpack.allowFilter=true")
        .arg("-c")
        .arg("uploadpackfilter.allow=false")
        .arg("-c")
        .arg("uploadpackfilter.blob:none.allow=true")
        .arg("-c")
        .arg("uploadpack.allowReachableSHA1InWant=true")
        .arg("upload-pack")
        .args(args)
        .arg(&view.path)
        .stdin(if request.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &view.global_config)
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1");

    // Do not let ambient Git repository variables redirect upload-pack away
    // from the private materialized view or add alternate object stores.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_TEMPLATE_DIR",
        "GIT_EXEC_PATH",
        "GIT_TRACE",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_SETUP",
    ] {
        command.env_remove(key);
    }
    if let Some(protocol) = git_protocol {
        command.env("GIT_PROTOCOL", protocol);
    } else {
        command.env_remove("GIT_PROTOCOL");
    }

    #[cfg(feature = "smart-http-diagnostics")]
    let subprocess_started = Instant::now();
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_subprocess_start",
        &[
            ("operation", serde_json::json!("upload_pack")),
            (
                "request_bytes",
                serde_json::json!(request.map_or(0, <[u8]>::len)),
            ),
            (
                "deadline_remaining_ms",
                serde_json::json!(deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()),
            ),
        ],
    );
    let mut child =
        crate::util::process::ManagedChild::spawn(&mut command, Some(remaining(deadline)?))
            .map_err(|e| Error::Invalid(format!("cannot start Git upload-pack: {e}")))?;
    let mut stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| Error::Bug("upload-pack stdout was not piped".into()))?;
    let read_limit = max_response_bytes.saturating_add(1);
    let mut output = Vec::new();
    let pid = child.child_mut().id();
    let (read_result, write_result) = if let Some(request) = request {
        let stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| Error::Bug("upload-pack stdin was not piped".into()))?;
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                let mut stdin = stdin;
                stdin.write_all(request)
            });
            let read_result = (&mut stdout).take(read_limit).read_to_end(&mut output);
            if output.len() as u64 > max_response_bytes {
                crate::util::process::terminate_process_tree(pid);
            }
            let write_result = writer.join().unwrap_or_else(|_| {
                Err(std::io::Error::other("upload-pack input writer panicked"))
            });
            (read_result, write_result)
        })
    } else {
        let read_result = (&mut stdout).take(read_limit).read_to_end(&mut output);
        if output.len() as u64 > max_response_bytes {
            crate::util::process::terminate_process_tree(pid);
        }
        (read_result, Ok(()))
    };
    if output.len() as u64 > max_response_bytes {
        let _ = child.wait();
        return Err(Error::Limit(format!(
            "Git upload-pack response exceeds the configured {} byte limit",
            max_response_bytes
        )));
    }
    if let Err(e) = read_result {
        return Err(Error::Protocol(format!(
            "could not read Git upload-pack response: {e}"
        )));
    }
    if let Err(e) = write_result {
        return Err(Error::Protocol(format!(
            "could not pass request to Git upload-pack: {e}"
        )));
    }
    let (status, timed_out) = child
        .wait()
        .map_err(|e| Error::Invalid(format!("could not wait for Git upload-pack: {e}")))?;
    #[cfg(feature = "smart-http-diagnostics")]
    crate::remote::diagnostics::event(
        "git_subprocess_end",
        &[
            ("operation", serde_json::json!("upload_pack")),
            (
                "elapsed_ms",
                serde_json::json!(subprocess_started.elapsed().as_millis()),
            ),
            ("output_bytes", serde_json::json!(output.len())),
            ("exit_code", serde_json::json!(status.code())),
            ("timed_out", serde_json::json!(timed_out)),
            (
                "timeout_reason",
                serde_json::json!(if timed_out {
                    "git_operation_deadline"
                } else {
                    "none"
                }),
            ),
        ],
    );
    if timed_out {
        return Err(git_deadline_error());
    }
    if !status.success() {
        return Err(Error::Protocol(format!(
            "Git upload-pack exited with status {status}"
        )));
    }
    Ok(output)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(git_deadline_error)
}

fn git_deadline_error() -> Error {
    Error::from(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Git smart-HTTP operation exceeded its 120-second deadline",
    ))
}

#[cfg(test)]
mod tests {
    use super::validate_git_protocol;

    #[test]
    fn git_protocol_versions_are_bounded_and_validated() {
        assert_eq!(validate_git_protocol(None).unwrap(), None);
        assert_eq!(validate_git_protocol(Some("version=0")).unwrap(), None);
        assert_eq!(
            validate_git_protocol(Some("version=1")).unwrap(),
            Some("version=1")
        );
        assert_eq!(
            validate_git_protocol(Some("version=2")).unwrap(),
            Some("version=2")
        );
        for invalid in [
            "version=3",
            "version=2:agent=git/2.43.0",
            "version=2\r\nX-Evil: yes",
        ] {
            assert!(validate_git_protocol(Some(invalid)).is_err(), "{invalid:?}");
        }
    }
}

#[cfg(test)]
#[path = "git_http_bench.rs"]
mod benchmark;
