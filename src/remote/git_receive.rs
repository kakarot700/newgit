//! Write-side Git smart-HTTP adapter for a deliberately narrow receive-pack slice.
//!
//! Git receives and validates the pack in a short-lived isolated projection.
//! The accepted branch tip is then imported into a temporary NewGit repository,
//! reusing exported canonical commit IDs. Only a single branch create or
//! fast-forward update is promoted, under NewGit's ref transaction lock.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::object::ObjectId;
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

/// This receive-pack adapter currently implements the conventional v0 HTTP
/// exchange only. Reject an explicit alternate protocol rather than replying
/// with the wrong wire framing.
pub fn validate_git_protocol(value: Option<&str>) -> Result<()> {
    match value.map(str::trim) {
        None | Some("") | Some("version=0") => Ok(()),
        Some(other) => Err(Error::Protocol(format!(
            "Git receive-pack supports protocol version 0 only, got {other:?}"
        ))),
    }
}

/// Return the standard smart-HTTP service advertisement using Git's own
/// receive-pack implementation over an isolated NewGit-backed projection.
pub fn advertise(repo: &Repo, max_response_bytes: u64) -> Result<Vec<u8>> {
    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let (view, _) = TempGitView::from_newgit_with_export(repo, deadline)?;
    let mut command = receive_pack_command(&view, true);
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

/// Accept exactly one create/fast-forward branch update. Git handles packfile
/// decoding and fsck in the disposable projection; NewGit's canonical objects
/// and refs are changed only after import/validation succeeds and the observed
/// old canonical tip still passes a CAS check under the transaction lock.
pub fn receive_pack(
    repo: &Repo,
    request: &[u8],
    max_response_bytes: u64,
    principal: &str,
) -> Result<Vec<u8>> {
    let push = parse_push_command(request)?;
    let branch_suffix = push.ref_name.strip_prefix("refs/heads/").ok_or_else(|| {
        Error::Invalid(
            "Git push supports refs/heads/* only; tags and other refs are refused".into(),
        )
    })?;
    if branch_suffix.is_empty() {
        return Err(Error::Invalid("Git push branch name is empty".into()));
    }
    crate::repo::refs::check_ref_name(&format!("refs/{branch_suffix}"))?;

    let deadline = Instant::now() + GIT_OPERATION_TIMEOUT;
    let (view, export) = TempGitView::from_newgit_with_export(repo, deadline)?;
    let initial_git_tip = git_ref_oid(&view, &push.ref_name, deadline)?;
    let request_old_matches = if push.old_oid == ZERO_SHA1 {
        initial_git_tip.is_none()
    } else {
        initial_git_tip.as_deref() == Some(push.old_oid.as_str())
    };

    let mut command = receive_pack_command(&view, false);
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

    // A stale old ID or a policy rejection is already reported by Git in the
    // valid receive-pack response. Never translate such a request into NewGit.
    if !request_old_matches {
        return Ok(response);
    }
    let final_git_tip = git_ref_oid(&view, &push.ref_name, deadline)?;
    if final_git_tip.as_deref() != Some(push.new_oid.as_str()) {
        return Ok(response);
    }
    if push.old_oid == push.new_oid {
        return Ok(response); // ordinary up-to-date push; no canonical write
    }

    let expected_newgit = if push.old_oid == ZERO_SHA1 {
        None
    } else {
        Some(*export.git_commit_oids.get(&push.old_oid).ok_or_else(|| {
            Error::Conflict(format!(
                "the advertised Git tip {} has no canonical NewGit mapping",
                push.old_oid
            ))
        })?)
    };
    let git_to_newgit: HashMap<String, ObjectId> = export.git_commit_oids.clone();

    let current_name = export
        .refs_exported
        .iter()
        .find(|(_, git_name)| git_name == &push.ref_name)
        .map(|(newgit_name, _)| newgit_name.clone());
    let newgit_ref = current_name.unwrap_or_else(|| format!("refs/{branch_suffix}"));
    crate::repo::refs::check_ref_name(&newgit_ref)?;
    let actual_old = repo.refs.read_opt(&newgit_ref)?;
    if actual_old != expected_newgit {
        return Err(Error::CasFailed(format!(
            "Git push for {} was based on a stale NewGit ref",
            push.ref_name
        )));
    }
    if expected_newgit.is_none() {
        ensure_no_ref_name_conflict(repo, &newgit_ref)?;
    }

    // Re-import into a scratch NewGit repository. Existing projection commits
    // are mapped back to their canonical NewGit IDs, so the new snapshots keep
    // the original history instead of duplicating its base.
    let stage_dir = tempfile::tempdir().map_err(Error::from)?;
    let staged = Repo::init_with(stage_dir.path(), repo.config.clone())?;
    crate::gitio::import::import_git_with_base_map(&staged, &view.path, &git_to_newgit, repo)?;
    let new_tip = staged.refs.read_opt(&push.ref_name)?.ok_or_else(|| {
        Error::Invalid(format!(
            "Git receive-pack accepted {} but its imported branch is missing",
            push.ref_name
        ))
    })?;
    if !matches!(
        object_from_either_repo(repo, &staged, new_tip)?,
        crate::object::types::Object::Snapshot(_)
    ) {
        return Err(Error::Invalid(
            "Git branch tip did not import as a NewGit snapshot".into(),
        ));
    }
    if let Some(old_tip) = expected_newgit {
        // receive.denyNonFastForwards is enforced by Git before it returns an
        // accepted status. The importer also proved every old projection commit
        // maps back to the canonical parent ID before producing new snapshots.
        debug_assert_ne!(old_tip, new_tip);
    }

    let staged_objects = staged_object_closure(repo, &staged, new_tip, max_response_bytes)?;
    let ops = vec![TxnOp::Ref {
        name: newgit_ref.clone(),
        cas: Cas::Exactly(expected_newgit),
        new: Some(new_tip),
        log: RefLogEntry::system(format!("Git push by {principal}")),
    }];
    txn::execute_with_precommit(repo.ng(), ops, repo.limits(), || {
        // A competing create may have introduced a file/directory ref-name
        // collision after the projection was exported. Recheck under the
        // transaction lock, before any staged object is promoted.
        if expected_newgit.is_none() {
            ensure_no_ref_name_conflict(repo, &newgit_ref)?;
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

fn parse_push_command(request: &[u8]) -> Result<PushCommand> {
    let mut offset = 0usize;
    let mut commands = Vec::new();
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
        if !ref_name.starts_with("refs/heads/") {
            return Err(Error::Invalid(
                "Git push supports refs/heads/* only; tags and other refs are refused".into(),
            ));
        }
        if fields[1] == ZERO_SHA1 {
            return Err(Error::Invalid(
                "Git branch deletion is not supported".into(),
            ));
        }
        commands.push(PushCommand {
            old_oid: fields[0].to_string(),
            new_oid: fields[1].to_string(),
            ref_name: ref_name.to_string(),
        });
        if commands.len() > 1 {
            return Err(Error::Invalid(
                "one Git branch may be pushed per request; multi-ref pushes are refused".into(),
            ));
        }
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
    Ok(commands.remove(0))
}

fn validate_sha1(oid: &str) -> Result<()> {
    if oid.len() != 40 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Protocol(
            "receive-pack requires 40-hex SHA-1 object IDs".into(),
        ));
    }
    Ok(())
}

fn receive_pack_command(view: &TempGitView, advertise: bool) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "receive.denyNonFastForwards=true",
            "-c",
            "receive.denyDeletes=true",
            "-c",
            "receive.fsckObjects=true",
            "-c",
            "receive.denyCurrentBranch=ignore",
            "-c",
            "receive.advertiseAtomic=false",
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
            "could not inspect Git branch {ref_name}: {status}"
        )));
    }
    let oid = String::from_utf8(output)
        .map_err(|_| Error::Protocol("Git returned a non-UTF-8 ref ID".into()))?
        .trim()
        .to_ascii_lowercase();
    validate_sha1(&oid)?;
    Ok(Some(oid))
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

fn staged_object_closure(
    canonical: &Repo,
    staged: &Repo,
    tip: ObjectId,
    max_total_bytes: u64,
) -> Result<Vec<(ObjectId, Vec<u8>)>> {
    let mut pending = vec![tip];
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
    use super::{ensure_no_ref_name_conflict, parse_push_command, validate_sha1, ZERO_SHA1};
    use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
    use crate::repo::Repo;

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
    fn parses_one_branch_update_followed_by_pack() {
        let new = "1234567890123456789012345678901234567890";
        let body =
            request(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        let parsed = parse_push_command(&body).unwrap();
        assert_eq!(parsed.old_oid, ZERO_SHA1);
        assert_eq!(parsed.new_oid, new);
        assert_eq!(parsed.ref_name, "refs/heads/main");
        assert!(parse_push_command(b"0000").is_err());
    }

    #[test]
    fn refuses_deletions_tags_and_multiple_refs_before_git_unpack() {
        let new = "1234567890123456789012345678901234567890";
        let deletion =
            request(format!("{new} {ZERO_SHA1} refs/heads/main\0report-status\n").as_bytes());
        assert!(parse_push_command(&deletion).is_err());
        let tag = request(format!("{ZERO_SHA1} {new} refs/tags/v1\0report-status\n").as_bytes());
        assert!(parse_push_command(&tag).is_err());
        let mut multi =
            pkt(format!("{ZERO_SHA1} {new} refs/heads/main\0report-status\n").as_bytes());
        multi.extend_from_slice(&pkt(
            format!("{ZERO_SHA1} {new} refs/heads/other\n").as_bytes()
        ));
        multi.extend_from_slice(b"0000PACK");
        assert!(parse_push_command(&multi).is_err());
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
        assert_eq!(parsed.new_oid, new);
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
