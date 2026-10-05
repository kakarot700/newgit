//! Write-side Git smart-HTTP adapter for a deliberately narrow receive-pack slice.
//!
//! Git receives and validates the pack in a short-lived isolated projection.
//! Accepted branch tips and lightweight tags are then imported into a temporary
//! NewGit repository, reusing exported canonical commit IDs. Branch creates,
//! fast-forward and forced non-fast-forward updates, deletions, and tag
//! creates/deletions are promoted under one NewGit ref transaction.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::object::ObjectId;
use crate::remote::auth::Role;
use crate::remote::git_http::{self, TempGitView};
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;

const GIT_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const RECEIVE_PACK_ANNOUNCEMENT: &[u8] = b"001f# service=git-receive-pack\n0000";
const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";

#[derive(Debug)]
struct PushCommand {
    old_oid: String,
    new_oid: String,
    ref_name: String,
}

/// Receive-pack uses the conventional push exchange for v0 and v1; v1 adds
/// its version packet to the advertisement. A v2 request is deliberately
/// downgraded to v0 because Git has no receive-pack v2 push command here.
pub fn validate_git_protocol(value: Option<&str>) -> Result<Option<&'static str>> {
    match value.map(str::trim) {
        None | Some("") | Some("version=0") | Some("version=2") => Ok(None),
        Some("version=1") => Ok(Some("version=1")),
        Some(other) => Err(Error::Protocol(format!(
            "Git receive-pack supports protocol versions 0 and 1 only, got {other:?}"
        ))),
    }
}

/// Return the standard smart-HTTP service advertisement using Git's own
/// receive-pack implementation over an isolated NewGit-backed projection.
pub fn advertise(
    repo: &Repo,
    git_protocol: Option<&str>,
    max_response_bytes: u64,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let (view, _) = TempGitView::from_newgit_with_export(repo, deadline)?;
    let mut command = receive_pack_command(&view, true, git_protocol);
    let (status, payload) = run_git(&view, &mut command, None, max_response_bytes, deadline)?;
    if !status.success() {
        return Err(Error::Protocol(format!(
            "Git receive-pack advertisement exited with status {status}"
        )));
    }
    let total = RECEIVE_PACK_ANNOUNCEMENT.len() as u64 + payload.len() as u64;
    if total > max_response_bytes {
        return Err(Error::Limit(format!(
            "Git receive-pack advertisement exceeds the configured {max_response_bytes} byte limit"
        )));
    }
    let mut response = Vec::with_capacity(total as usize);
    response.extend_from_slice(RECEIVE_PACK_ANNOUNCEMENT);
    response.extend_from_slice(&payload);
    Ok(response)
}

/// Accept branch creates, fast-forward or forced non-fast-forward updates, and
/// deletions plus lightweight tag creates/deletions. The Git wire command does
/// not identify whether a client used `--force`; standard Git clients enforce
/// that choice locally. Git handles packfile decoding and fsck in the disposable
/// projection; NewGit's canonical objects and refs change only after
/// import/validation succeeds and every observed old canonical tip still passes
/// a CAS check under the lock.
pub fn receive_pack(
    repo: &Repo,
    request: &[u8],
    git_protocol: Option<&str>,
    max_response_bytes: u64,
    principal: &str,
    role: Role,
    protected_refs: &HashSet<String>,
) -> Result<Vec<u8>> {
    let pushes = parse_push_command(request)?;
    authorize_protected_updates(&pushes, protected_refs, role)?;
    for push in &pushes {
        let newgit_ref = git_ref_fallback_newgit_name(&push.ref_name)?;
        crate::repo::refs::check_ref_name(&newgit_ref)?;
        if push.ref_name.starts_with("refs/tags/")
            && push.old_oid != ZERO_SHA1
            && push.new_oid != ZERO_SHA1
            && push.old_oid != push.new_oid
        {
            return Err(Error::Conflict(
                "updating an existing Git tag is refused; delete it and create a new tag instead"
                    .into(),
            ));
        }
    }

    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let (view, export) = TempGitView::from_newgit_with_export(repo, deadline)?;
    let git_to_newgit: HashMap<String, ObjectId> = export.git_commit_oids.clone();
    let mut updates = Vec::with_capacity(pushes.len());
    let mut newgit_names = HashSet::new();
    for push in pushes {
        let newgit_ref = export
            .refs_exported
            .iter()
            .find(|(_, git_name)| git_name == &push.ref_name)
            .map(|(newgit_name, _)| newgit_name.clone())
            .unwrap_or(git_ref_fallback_newgit_name(&push.ref_name)?);
        crate::repo::refs::check_ref_name(&newgit_ref)?;
        if !newgit_names.insert(newgit_ref.clone()) {
            return Err(Error::Conflict(format!(
                "multiple Git refs map to the same NewGit ref {newgit_ref:?}"
            )));
        }
        let initial_git_tip = git_ref_oid(&view, &push.ref_name, deadline)?;
        let request_old_matches = if push.old_oid == ZERO_SHA1 {
            initial_git_tip.is_none()
        } else {
            initial_git_tip.as_deref() == Some(push.old_oid.as_str())
        };
        let expected_newgit = if push.old_oid == ZERO_SHA1 {
            None
        } else {
            Some(*git_to_newgit.get(&push.old_oid).ok_or_else(|| {
                Error::Conflict(format!(
                    "the advertised Git tip {} has no canonical NewGit mapping",
                    push.old_oid
                ))
            })?)
        };
        updates.push(PushUpdate {
            push,
            newgit_ref,
            request_old_matches,
            expected_newgit,
        });
    }

    ensure_batch_ref_names_do_not_conflict(&newgit_names)?;
    let mut command = receive_pack_command(&view, false, git_protocol);
    let (status, response) = run_git(
        &view,
        &mut command,
        Some(request),
        max_response_bytes,
        deadline,
    )?;
    if !status.success() {
        return Err(Error::Protocol(format!(
            "Git receive-pack exited with status {status}"
        )));
    }

    // Git may accept some commands in an ordinary (non-atomic) protocol
    // request while rejecting others. Never promote such a partial projection:
    // NewGit refs are committed together below, and HTTP failure avoids
    // reporting a projected success that was not made canonical.
    let mut accepted = 0usize;
    for update in &updates {
        let final_git_tip = git_ref_oid(&view, &update.push.ref_name, deadline)?;
        let final_tip_matches = if update.push.new_oid == ZERO_SHA1 {
            final_git_tip.is_none()
        } else {
            final_git_tip.as_deref() == Some(update.push.new_oid.as_str())
        };
        if update.request_old_matches && final_tip_matches {
            accepted += 1;
        }
    }
    if accepted != updates.len() {
        if accepted > 0 {
            return Err(Error::Conflict(
                "Git accepted only part of this multi-ref push; no NewGit refs were changed".into(),
            ));
        }
        return Ok(response);
    }

    // NewGit represents lightweight tags as refs to snapshots, not Git tag
    // objects. Check the target type before import or canonical object staging.
    for update in &updates {
        if update.push.ref_name.starts_with("refs/tags/") && update.push.new_oid != ZERO_SHA1 {
            let object_type = git_object_type(&view, &update.push.new_oid, deadline)?;
            validate_lightweight_tag_target(&update.push.ref_name, &object_type)?;
        }
    }

    let changed: Vec<&PushUpdate> = updates
        .iter()
        .filter(|update| update.push.old_oid != update.push.new_oid)
        .collect();
    if changed.is_empty() {
        return Ok(response); // ordinary up-to-date push; no canonical write
    }
    for update in &changed {
        let actual_old = repo.refs.read_opt(&update.newgit_ref)?;
        if actual_old != update.expected_newgit {
            return Err(Error::CasFailed(format!(
                "Git push for {} was based on a stale NewGit ref",
                update.push.ref_name
            )));
        }
        if update.expected_newgit.is_none() {
            ensure_no_ref_name_conflict(repo, &update.newgit_ref)?;
        }
    }

    // Re-import into a scratch NewGit repository. Existing projection commits
    // are mapped back to their canonical NewGit IDs, so the new snapshots keep
    // the original history instead of duplicating its base.
    let stage_dir = tempfile::tempdir().map_err(Error::from)?;
    let staged = Repo::init_with(stage_dir.path(), repo.config.clone())?;
    crate::gitio::import::import_git_with_base_map(&staged, &view.path, &git_to_newgit, repo)?;
    let mut new_tips = Vec::with_capacity(changed.len());
    let mut ops = Vec::with_capacity(changed.len());
    let mut created_names = Vec::new();
    for update in &changed {
        let new_tip = if update.push.new_oid == ZERO_SHA1 {
            None
        } else {
            let new_tip = staged
                .refs
                .read_opt(&update.push.ref_name)?
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "Git receive-pack accepted {} but its imported ref is missing",
                        update.push.ref_name
                    ))
                })?;
            if !matches!(
                object_from_either_repo(repo, &staged, new_tip)?,
                crate::object::types::Object::Snapshot(_)
            ) {
                return Err(Error::Invalid(
                    "Git ref tip did not import as a NewGit snapshot".into(),
                ));
            }
            if let Some(old_tip) = update.expected_newgit {
                // receive.denyNonFastForwards is enforced by Git before it
                // returns an accepted status. The importer also proves every
                // old projection commit maps back to the canonical parent ID.
                debug_assert_ne!(Some(old_tip), Some(new_tip));
            } else {
                created_names.push(update.newgit_ref.clone());
            }
            new_tips.push(new_tip);
            Some(new_tip)
        };
        ops.push(TxnOp::Ref {
            name: update.newgit_ref.clone(),
            cas: Cas::Exactly(update.expected_newgit),
            new: new_tip,
            log: RefLogEntry::system(format!("Git push by {principal}")),
        });
    }

    let staged_objects = staged_object_closures(repo, &staged, &new_tips, max_response_bytes)?;
    txn::execute_with_precommit(repo.ng(), ops, repo.limits(), || {
        // A competing create may have introduced a file/directory ref-name
        // collision after projection. Recheck every creation under the
        // transaction lock, before any staged object is promoted.
        for name in &created_names {
            ensure_no_ref_name_conflict(repo, name)?;
        }
        for (oid, canonical) in &staged_objects {
            let stored = repo.objects.put_canonical(canonical)?;
            if stored != *oid {
                return Err(Error::Bug(format!(
                    "staged object identity changed during Git push promotion: {oid} != {stored}"
                )));
            }
        }
        Ok(())
    })?;
    Ok(response)
}

fn validate_lightweight_tag_target(ref_name: &str, object_type: &str) -> Result<()> {
    match object_type {
        "commit" => Ok(()),
        "tag" => Err(Error::Invalid(format!(
            "annotated Git tag {ref_name} resolves to a tag object; NewGit has no Git tag-object or per-ref metadata representation, so tagger/message/signature data cannot be preserved"
        ))),
        other => Err(Error::Invalid(format!(
            "Git tag {ref_name} targets a {other} object; only lightweight tags directly targeting commits are supported"
        ))),
    }
}

#[derive(Debug)]
struct PushUpdate {
    push: PushCommand,
    newgit_ref: String,
    request_old_matches: bool,
    expected_newgit: Option<ObjectId>,
}

fn ensure_batch_ref_names_do_not_conflict(names: &HashSet<String>) -> Result<()> {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    for (index, name) in names.iter().enumerate() {
        if names.iter().skip(index + 1).any(|other| {
            other
                .strip_prefix(name)
                .is_some_and(|tail| tail.starts_with('/'))
                || name
                    .strip_prefix(other)
                    .is_some_and(|tail| tail.starts_with('/'))
        }) {
            return Err(Error::Conflict(
                "multi-ref push contains conflicting NewGit ref names".into(),
            ));
        }
    }
    Ok(())
}

fn parse_push_command(request: &[u8]) -> Result<Vec<PushCommand>> {
    let mut offset = 0usize;
    let mut commands = Vec::new();
    let mut ref_names = HashSet::new();
    let mut first = true;
    loop {
        if request.len().saturating_sub(offset) < 4 {
            return Err(Error::Protocol(
                "truncated receive-pack pkt-line header".into(),
            ));
        }
        let length_text = std::str::from_utf8(&request[offset..offset + 4])
            .map_err(|_| Error::Protocol("non-ASCII receive-pack pkt-line length".into()))?;
        let length = usize::from_str_radix(length_text, 16)
            .map_err(|_| Error::Protocol("invalid receive-pack pkt-line length".into()))?;
        offset += 4;
        if length == 0 {
            break;
        }
        if length < 4 || length > request.len().saturating_sub(offset) + 4 {
            return Err(Error::Protocol(
                "invalid or truncated receive-pack pkt-line".into(),
            ));
        }
        let end = offset + length - 4;
        let packet = &request[offset..end];
        offset = end;
        let command_bytes = packet.split(|byte| *byte == 0).next().unwrap_or_default();
        let command_line = command_bytes.strip_suffix(b"\n").unwrap_or(command_bytes);
        let command_text = std::str::from_utf8(command_line)
            .map_err(|_| Error::Protocol("receive-pack command is not UTF-8".into()))?;
        if first && command_text.starts_with("push-cert ") {
            return Err(Error::Invalid("signed Git pushes are not supported".into()));
        }
        first = false;
        let fields: Vec<&str> = command_text.split(' ').collect();
        if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
            return Err(Error::Protocol(
                "malformed receive-pack update command".into(),
            ));
        }
        validate_sha1(fields[0])?;
        validate_sha1(fields[1])?;
        let ref_name = fields[2];
        if !ref_name.starts_with("refs/heads/") && !ref_name.starts_with("refs/tags/") {
            return Err(Error::Invalid(
                "Git push supports refs/heads/* and lightweight refs/tags/* only; other refs are refused".into(),
            ));
        }
        if fields[0] == ZERO_SHA1 && fields[1] == ZERO_SHA1 {
            return Err(Error::Invalid(
                "receive-pack command cannot create or delete a ref from the zero object ID".into(),
            ));
        }
        if !ref_names.insert(ref_name.to_string()) {
            return Err(Error::Invalid(format!(
                "receive-pack request contains duplicate update for {ref_name}"
            )));
        }
        commands.push(PushCommand {
            old_oid: fields[0].to_string(),
            new_oid: fields[1].to_string(),
            ref_name: ref_name.to_string(),
        });
    }
    if commands.is_empty() {
        return Err(Error::Protocol(
            "receive-pack request contains no ref updates".into(),
        ));
    }
    // Git may legitimately omit the pack when a newly created ref points to
    // an object already present on the server. Non-empty trailing data must
    // still be a pack; Git validates its contents and reachability.
    if !request[offset..].is_empty() && !request[offset..].starts_with(b"PACK") {
        return Err(Error::Protocol(
            "receive-pack update has invalid trailing data (expected a Git packfile)".into(),
        ));
    }
    Ok(commands)
}

fn validate_sha1(oid: &str) -> Result<()> {
    if oid.len() != 40 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Protocol(
            "receive-pack requires 40-hex SHA-1 object IDs".into(),
        ));
    }
    Ok(())
}

/// Validate one exact Git ref name supported by the receive-pack adapter.
/// Wildcards, prefixes, and other namespaces are not policy patterns here.
pub fn validate_protected_ref(ref_name: &str) -> Result<()> {
    if !ref_name.starts_with("refs/heads/") && !ref_name.starts_with("refs/tags/") {
        return Err(Error::Invalid(format!(
            "protected ref must be an exact supported Git ref under refs/heads/ or refs/tags/: {ref_name:?}"
        )));
    }
    if ref_name.contains("..") || ref_name.contains("@{") || ref_name == "@" {
        return Err(Error::Invalid(format!(
            "protected ref must be an exact valid Git ref name: {ref_name:?}"
        )));
    }
    if ref_name.bytes().any(|byte| {
        byte <= b' '
            || byte == 0x7f
            || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return Err(Error::Invalid(format!(
            "protected ref must be an exact valid Git ref name: {ref_name:?}"
        )));
    }
    if ref_name.split('/').any(|segment| {
        segment.is_empty()
            || segment.starts_with('.')
            || segment.ends_with('.')
            || segment.to_ascii_lowercase().ends_with(".lock")
    }) {
        return Err(Error::Invalid(format!(
            "protected ref must be an exact valid Git ref name: {ref_name:?}"
        )));
    }
    let newgit_ref = git_ref_fallback_newgit_name(ref_name)?;
    crate::repo::refs::check_ref_name(&newgit_ref).map_err(|error| {
        Error::Invalid(format!(
            "protected Git ref {ref_name:?} is not representable by NewGit: {error}"
        ))
    })
}

fn authorize_protected_updates(
    pushes: &[PushCommand],
    protected_refs: &HashSet<String>,
    role: Role,
) -> Result<()> {
    // Git branch names `refs/heads/tags/X` and tag names `refs/tags/X` both
    // map to NewGit `refs/tags/X`. Protect the canonical destination too so
    // an unprotected wire-name alias cannot bypass a protected ref policy.
    let protected_newgit_refs = protected_refs
        .iter()
        .map(|ref_name| {
            validate_protected_ref(ref_name)?;
            git_ref_fallback_newgit_name(ref_name)
        })
        .collect::<Result<HashSet<_>>>()?;
    for push in pushes {
        validate_protected_ref(&push.ref_name)?;
        let newgit_ref = git_ref_fallback_newgit_name(&push.ref_name)?;
        if push.old_oid != push.new_oid
            && protected_newgit_refs.contains(&newgit_ref)
            && role != Role::Admin
        {
            return Err(Error::Forbidden(format!(
                "admin role is required to change protected Git ref {:?}",
                push.ref_name
            )));
        }
    }
    Ok(())
}

fn git_ref_fallback_newgit_name(ref_name: &str) -> Result<String> {
    if let Some(suffix) = ref_name.strip_prefix("refs/heads/") {
        if suffix.is_empty() {
            return Err(Error::Invalid("Git push branch name is empty".into()));
        }
        Ok(format!("refs/{suffix}"))
    } else if let Some(suffix) = ref_name.strip_prefix("refs/tags/") {
        if suffix.is_empty() {
            return Err(Error::Invalid("Git push tag name is empty".into()));
        }
        Ok(format!("refs/tags/{suffix}"))
    } else {
        Err(Error::Invalid(
            "Git push supports refs/heads/* and lightweight refs/tags/* only; other refs are refused"
                .into(),
        ))
    }
}

fn receive_pack_command(
    view: &TempGitView,
    advertise: bool,
    git_protocol: Option<&str>,
) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "receive.denyNonFastForwards=false",
            "-c",
            "receive.denyDeletes=false",
            "-c",
            "receive.fsckObjects=true",
            "-c",
            "receive.denyCurrentBranch=ignore",
            "-c",
            "receive.advertiseAtomic=true",
            "-c",
            "receive.advertisePushOptions=false",
            "receive-pack",
        ])
        .arg("--stateless-rpc");
    if advertise {
        command.arg("--advertise-refs");
    }
    command
        .arg(&view.path)
        .stdin(if advertise {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &view.global_config)
        .env("GIT_CONFIG_COUNT", "0");
    git_http::isolate_git_environment(&mut command);
    if let Some(protocol) = git_protocol {
        command.env("GIT_PROTOCOL", protocol);
    }
    command
}

fn run_git(
    _view: &TempGitView,
    command: &mut Command,
    input: Option<&[u8]>,
    max_output_bytes: u64,
    deadline: Instant,
) -> Result<(ExitStatus, Vec<u8>)> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(git_deadline_error)?;
    let mut child = crate::util::process::ManagedChild::spawn(command, Some(remaining))
        .map_err(|error| Error::Invalid(format!("cannot start Git receive-pack: {error}")))?;
    let mut stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| Error::Bug("receive-pack stdout was not piped".into()))?;
    let read_limit = max_output_bytes.saturating_add(1);
    let mut output = Vec::new();
    let pid = child.child_mut().id();
    let (read_result, write_result) = if let Some(input) = input {
        let stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| Error::Bug("receive-pack stdin was not piped".into()))?;
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                let mut stdin = stdin;
                stdin.write_all(input)
            });
            let read_result = (&mut stdout).take(read_limit).read_to_end(&mut output);
            if output.len() as u64 > max_output_bytes {
                crate::util::process::terminate_process_tree(pid);
            }
            let write_result = writer
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("receive-pack writer panicked")));
            (read_result, write_result)
        })
    } else {
        let read_result = (&mut stdout).take(read_limit).read_to_end(&mut output);
        if output.len() as u64 > max_output_bytes {
            crate::util::process::terminate_process_tree(pid);
        }
        (read_result, Ok(()))
    };
    let (status, timed_out) = child
        .wait()
        .map_err(|error| Error::Invalid(format!("could not wait for Git receive-pack: {error}")))?;
    if timed_out {
        return Err(git_deadline_error());
    }
    if output.len() as u64 > max_output_bytes {
        return Err(Error::Limit(format!(
            "Git receive-pack output exceeds the configured {max_output_bytes} byte limit"
        )));
    }
    read_result.map_err(|error| {
        Error::Protocol(format!("could not read Git receive-pack output: {error}"))
    })?;
    write_result.map_err(|error| {
        Error::Protocol(format!(
            "could not pass request to Git receive-pack: {error}"
        ))
    })?;
    Ok((status, output))
}

fn git_ref_oid(view: &TempGitView, ref_name: &str, deadline: Instant) -> Result<Option<String>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&view.path)
        .args(["show-ref", "--verify", "--hash"])
        .arg(ref_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &view.global_config)
        .env("GIT_CONFIG_COUNT", "0");
    git_http::isolate_git_environment(&mut command);
    let (status, output) = run_git(view, &mut command, None, 128, deadline)?;
    if !status.success() {
        if matches!(status.code(), Some(1) | Some(128)) {
            return Ok(None);
        }
        return Err(Error::Protocol(format!(
            "could not inspect Git ref {ref_name}: {status}"
        )));
    }
    let oid = String::from_utf8(output)
        .map_err(|_| Error::Protocol("Git returned a non-UTF-8 ref ID".into()))?
        .trim()
        .to_ascii_lowercase();
    validate_sha1(&oid)?;
    Ok(Some(oid))
}

fn git_object_type(view: &TempGitView, oid: &str, deadline: Instant) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&view.path)
        .args(["cat-file", "-t", oid])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &view.global_config)
        .env("GIT_CONFIG_COUNT", "0");
    git_http::isolate_git_environment(&mut command);
    let (status, output) = run_git(view, &mut command, None, 128, deadline)?;
    if !status.success() {
        return Err(Error::Protocol(format!(
            "could not inspect Git object {oid}: {status}"
        )));
    }
    String::from_utf8(output)
        .map(|value| value.trim().to_string())
        .map_err(|_| Error::Protocol("Git returned a non-UTF-8 object type".into()))
}

fn ensure_no_ref_name_conflict(repo: &Repo, wanted: &str) -> Result<()> {
    for (existing, _) in repo.refs.list(None)? {
        if existing == wanted {
            continue;
        }
        if existing
            .strip_prefix(wanted)
            .is_some_and(|tail| tail.starts_with('/'))
            || wanted
                .strip_prefix(&existing)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return Err(Error::Conflict(format!(
                "NewGit ref {wanted:?} conflicts with existing ref {existing:?}"
            )));
        }
    }
    Ok(())
}

fn object_from_either_repo(
    canonical: &Repo,
    staged: &Repo,
    oid: ObjectId,
) -> Result<crate::object::types::Object> {
    match canonical.objects.get(&oid) {
        Ok(object) => Ok(object),
        Err(Error::NotFound(_)) => staged.objects.get(&oid),
        Err(error) => Err(error),
    }
}

fn staged_object_closures(
    canonical: &Repo,
    staged: &Repo,
    tips: &[ObjectId],
    max_total_bytes: u64,
) -> Result<Vec<(ObjectId, Vec<u8>)>> {
    let mut pending = tips.to_vec();
    let mut visited = HashSet::new();
    let mut objects = Vec::new();
    let mut total = 0u64;
    while let Some(oid) = pending.pop() {
        if !visited.insert(oid) {
            continue;
        }
        match canonical.objects.get(&oid) {
            Ok(_) => continue,
            Err(Error::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
        let object = staged.objects.get(&oid)?;
        let canonical_bytes = staged.objects.get_canonical(&oid)?;
        total = total
            .checked_add(canonical_bytes.len() as u64)
            .ok_or_else(|| Error::Limit("staged Git object size overflow".into()))?;
        if total > max_total_bytes {
            return Err(Error::Limit(format!(
                "translated Git object closure exceeds the configured {max_total_bytes} byte limit"
            )));
        }
        match &object {
            crate::object::types::Object::Snapshot(snapshot) => {
                pending.extend(snapshot.parents.iter().copied());
                pending.extend([snapshot.root, snapshot.author]);
                pending.extend(snapshot.change);
                pending.extend(snapshot.goal);
            }
            crate::object::types::Object::Tree(tree) => {
                pending.extend(tree.entries.iter().map(|entry| entry.oid));
            }
            crate::object::types::Object::Blob(_) | crate::object::types::Object::Actor(_) => {}
            other => {
                return Err(Error::Invalid(format!(
                    "Git import produced unsupported staged object type {}",
                    other.type_tag().name()
                )))
            }
        }
        objects.push((oid, canonical_bytes));
    }
    Ok(objects)
}

fn git_deadline_error() -> Error {
    Error::from(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Git receive-pack operation exceeded its 120-second deadline",
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        authorize_protected_updates, ensure_no_ref_name_conflict, parse_push_command,
        validate_git_protocol, validate_lightweight_tag_target, validate_protected_ref,
        validate_sha1, PushCommand, ZERO_SHA1,
    };
    use crate::error::Error;
    use crate::remote::auth::Role;
    use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
    use crate::repo::Repo;
    use std::collections::HashSet;

    fn pkt(payload: &[u8]) -> Vec<u8> {
        let length = payload.len() + 4;
        let mut out = format!("{length:04x}").into_bytes();
        out.extend_from_slice(payload);
        out
    }

    fn request(command: &[u8]) -> Vec<u8> {
        let mut out = pkt(command);
        out.extend_from_slice(b"0000PACK");
        out
    }

    #[test]
    fn receive_pack_supports_v0_v1_and_falls_back_from_v2() {
        assert_eq!(validate_git_protocol(None).unwrap(), None);
        assert_eq!(validate_git_protocol(Some("version=0")).unwrap(), None);
        assert_eq!(
            validate_git_protocol(Some("version=1")).unwrap(),
            Some("version=1")
        );
        assert_eq!(validate_git_protocol(Some("version=2")).unwrap(), None);
        assert!(validate_git_protocol(Some("version=3")).is_err());
    }

    #[test]
    fn annotated_tag_push_refusal_names_the_unrepresentable_metadata() {
        let error = validate_lightweight_tag_target("refs/tags/v1", "tag").unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid value: annotated Git tag refs/tags/v1 resolves to a tag object; NewGit has no Git tag-object or per-ref metadata representation, so tagger/message/signature data cannot be preserved"
        );
        assert!(validate_lightweight_tag_target("refs/tags/v1", "commit").is_ok());
        assert_eq!(
            validate_lightweight_tag_target("refs/tags/blob", "blob")
                .unwrap_err()
                .to_string(),
            "invalid value: Git tag refs/tags/blob targets a blob object; only lightweight tags directly targeting commits are supported"
        );
    }

    #[test]
    fn parses_one_branch_update_followed_by_pack() {
        let new = "1234567890123456789012345678901234567890";
        let body =
            request(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        let parsed = parse_push_command(&body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].old_oid, ZERO_SHA1);
        assert_eq!(parsed[0].new_oid, new);
        assert_eq!(parsed[0].ref_name, "refs/heads/main");
        assert!(parse_push_command(b"0000").is_err());
    }

    #[test]
    fn parses_multiple_distinct_branches_and_refuses_duplicate_updates() {
        let new = "1234567890123456789012345678901234567890";
        let second = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let mut multi =
            pkt(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        multi.extend_from_slice(&pkt(
            format!("{ZERO_SHA1} {second} refs/heads/other\n").as_bytes()
        ));
        multi.extend_from_slice(b"0000PACK");
        let parsed = parse_push_command(&multi).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].ref_name, "refs/heads/main");
        assert_eq!(parsed[1].ref_name, "refs/heads/other");

        let deletion =
            request(format!("{new} {ZERO_SHA1} refs/heads/main\0report-status\n").as_bytes());
        let parsed_deletion = parse_push_command(&deletion).unwrap();
        assert_eq!(parsed_deletion.len(), 1);
        assert_eq!(parsed_deletion[0].old_oid, new);
        assert_eq!(parsed_deletion[0].new_oid, ZERO_SHA1);
        let zero_to_zero =
            request(format!("{ZERO_SHA1} {ZERO_SHA1} refs/heads/main\0report-status\n").as_bytes());
        assert!(parse_push_command(&zero_to_zero).is_err());
        let tag = request(format!("{ZERO_SHA1} {new} refs/tags/v1\0report-status\n").as_bytes());
        assert_eq!(
            parse_push_command(&tag).unwrap()[0].ref_name,
            "refs/tags/v1"
        );
        let notes = request(format!("{ZERO_SHA1} {new} refs/notes/n1\0report-status\n").as_bytes());
        assert!(parse_push_command(&notes).is_err());
        let mut duplicate =
            pkt(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        duplicate.extend_from_slice(&pkt(
            format!("{ZERO_SHA1} {second} refs/heads/main\n").as_bytes()
        ));
        duplicate.extend_from_slice(b"0000PACK");
        assert!(parse_push_command(&duplicate).is_err());
    }

    #[test]
    fn rejects_malformed_pkt_lines_and_non_sha1_ids() {
        assert!(parse_push_command(b"000x").is_err());
        assert!(parse_push_command(b"0008x").is_err());
        assert!(validate_sha1("not-an-object-id").is_err());
    }

    #[test]
    fn permits_an_omitted_pack_for_an_object_already_on_the_server() {
        let new = "1234567890123456789012345678901234567890";
        let mut body =
            pkt(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        body.extend_from_slice(b"0000");
        let parsed = parse_push_command(&body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].new_oid, new);
    }

    #[test]
    fn protected_ref_configuration_accepts_exact_supported_names_only() {
        for ref_name in [
            "refs/heads/main",
            "refs/heads/release/2026.10",
            "refs/tags/v1.2.3",
        ] {
            assert!(validate_protected_ref(ref_name).is_ok(), "{ref_name:?}");
        }
        for ref_name in [
            "main",
            "refs/remotes/origin/main",
            "refs/heads/",
            "refs/heads/*",
            "refs/heads/main/*",
            "refs/heads/main?",
            "refs/heads/.hidden",
            "refs/heads/a..b",
            "refs/tags/release.lock",
        ] {
            assert!(validate_protected_ref(ref_name).is_err(), "{ref_name:?}");
        }
    }

    #[test]
    fn protected_ref_changes_require_admin_but_noops_and_neighboring_names_do_not() {
        let protected = HashSet::from([
            "refs/heads/main".to_string(),
            "refs/heads/release".to_string(),
            "refs/tags/release".to_string(),
        ]);
        let create = PushCommand {
            old_oid: ZERO_SHA1.into(),
            new_oid: "1111111111111111111111111111111111111111".into(),
            ref_name: "refs/heads/main".into(),
        };
        let update = PushCommand {
            old_oid: "1111111111111111111111111111111111111111".into(),
            new_oid: "2222222222222222222222222222222222222222".into(),
            ref_name: "refs/heads/main".into(),
        };
        let delete = PushCommand {
            old_oid: "1111111111111111111111111111111111111111".into(),
            new_oid: ZERO_SHA1.into(),
            ref_name: "refs/heads/release".into(),
        };
        let mapping_alias = PushCommand {
            old_oid: ZERO_SHA1.into(),
            new_oid: "3333333333333333333333333333333333333333".into(),
            ref_name: "refs/heads/tags/release".into(),
        };
        for change in [&create, &update, &delete, &mapping_alias] {
            assert!(matches!(
                authorize_protected_updates(std::slice::from_ref(change), &protected, Role::Write),
                Err(Error::Forbidden(_))
            ));
            assert!(authorize_protected_updates(
                std::slice::from_ref(change),
                &protected,
                Role::Admin
            )
            .is_ok());
        }

        let noop = PushCommand {
            old_oid: create.new_oid.clone(),
            new_oid: create.new_oid.clone(),
            ref_name: create.ref_name.clone(),
        };
        let neighboring_exact_name = PushCommand {
            old_oid: ZERO_SHA1.into(),
            new_oid: create.new_oid,
            ref_name: "refs/heads/mainline".into(),
        };
        assert!(authorize_protected_updates(
            &[noop, neighboring_exact_name],
            &protected,
            Role::Write
        )
        .is_ok());
        assert!(matches!(
            authorize_protected_updates(
                &[
                    update,
                    PushCommand {
                        old_oid: ZERO_SHA1.into(),
                        new_oid: "4444444444444444444444444444444444444444".into(),
                        ref_name: "refs/heads/other".into(),
                    },
                ],
                &protected,
                Role::Write
            ),
            Err(Error::Forbidden(_))
        ));
    }

    #[test]
    fn branch_prefix_conflict_skips_precommit_object_promotion() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repo::init(directory.path()).unwrap();
        let existing = crate::object::ObjectId::from_hex(
            "1111111111111111111111111111111111111111111111111111111111111111",
        )
        .unwrap();
        repo.refs
            .update(
                "refs/main/child",
                Cas::Any,
                Some(existing),
                RefLogEntry::system("existing sibling ref"),
            )
            .unwrap();
        let object = crate::object::types::Object::Blob(b"not promoted".to_vec());
        let object_id = object.id();
        let result = txn::execute_with_precommit(
            repo.ng(),
            vec![TxnOp::Ref {
                name: "refs/main".into(),
                cas: Cas::Exactly(None),
                new: Some(object_id),
                log: RefLogEntry::system("conflicting branch"),
            }],
            repo.limits(),
            || {
                ensure_no_ref_name_conflict(&repo, "refs/main")?;
                repo.objects.put(&object)?;
                Ok(())
            },
        );
        assert!(result.is_err(), "a prefix-conflicting ref must be rejected");
        assert_eq!(repo.refs.read("refs/main/child").unwrap(), existing);
        assert!(!repo
            .refs
            .list(None)
            .unwrap()
            .iter()
            .any(|(name, _)| name == "refs/main"));
        assert!(!repo.objects.contains(&object_id));
    }
}
