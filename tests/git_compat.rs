//! Git compatibility tests (iteration 8): REAL system git repositories are
//! imported into NewGit and exported back, then compared against git's own
//! view of the data (ls-tree, cat-file, log). Covers: branches, merges
//! (first-parent order), tags (annotated + lightweight), binary files,
//! symlinks, exec bits, unicode paths/messages, empty commits, multiple
//! authors + timezones, determinism, submodule refusal, empty repos, and
//! skipped git-internal namespaces.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use common::*;
use newgit::gitio::export::export_git;
use newgit::gitio::import::import_git;
use newgit::object::types::{EntryMode, Object};
use newgit::object::ObjectId;
use newgit::ops::tree::flatten_tree;
use newgit::ops::verify::{verify, VerifyOpts};
use newgit::repo::Repo;

// ─────────────────────────── git helpers ───────────────────────────

fn git(dir: &Path, args: &[&str]) -> Output {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        o.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(dir, args).stdout).to_string()
}

fn init_git(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet", "-b", "master"]);
    git(dir, &["config", "user.name", "Test Author"]);
    git(dir, &["config", "user.email", "author@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", msg]);
}

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

/// A rich git repo: merges, tags, binaries, symlink, exec bit, unicode,
/// empty commit, second author with a timezone.
fn rich_git_repo(dir: &Path) {
    init_git(dir);
    write(dir, "a.txt", b"alpha\n");
    write(dir, "sub/dir/b.bin", &[0u8, 1, 2, 3, 255, 254, 0, 128]);
    git(dir, &["add", "-A"]);
    git(dir, &["update-index", "--chmod=+x", "a.txt"]);
    git(dir, &["commit", "--quiet", "-m", "first commit"]);

    write(
        dir,
        "h\u{00e9}llo \u{00fc}nicode.txt",
        "namaste world\n".as_bytes(),
    );
    std::os::unix::fs::symlink("a.txt", dir.join("link.txt")).unwrap();
    git(dir, &["add", "-A"]);
    // one-shot second author + timezone + DIFFERENT committer (env-scoped)
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "commit",
            "--quiet",
            "-m",
            "unicode + symlink\n\nmulti-line body\n",
        ])
        .env("GIT_AUTHOR_NAME", "Second Author")
        .env("GIT_AUTHOR_EMAIL", "second@example.org")
        .env("GIT_AUTHOR_DATE", "2021-06-15T12:30:00+05:30")
        .env("GIT_COMMITTER_NAME", "Committer Different")
        .env("GIT_COMMITTER_EMAIL", "committer@example.net")
        .env("GIT_COMMITTER_DATE", "2021-06-16T09:00:00-08:00")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "second commit failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    git(dir, &["checkout", "--quiet", "-b", "feature"]);
    write(dir, "f.txt", b"feature work\n");
    git(dir, &["mv", "a.txt", "renamed-a.txt"]);
    commit(dir, "feature: rename + add");

    git(
        dir,
        &["commit", "--quiet", "--allow-empty", "-m", "empty commit"],
    );

    git(dir, &["checkout", "--quiet", "master"]);
    write(dir, "m.txt", b"master line\n");
    commit(dir, "master continues");

    // merge with a real conflict resolution (tree differs from both parents)
    git(dir, &["merge", "--no-ff", "--no-commit", "feature"]);
    // resolve: keep both sides' files; write a distinct merged file
    write(dir, "merged-note.txt", b"resolved by hand\n");
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "commit",
            "--quiet",
            "-m",
            "merge feature (conflict resolved differently from both sides)",
        ],
    );

    git(dir, &["tag", "light-tag"]);
    git(dir, &["tag", "-a", "v1.0", "-m", "annotated tag message"]);
    // git-internal refs that must be skipped by the importer
    git(
        dir,
        &[
            "update-ref",
            "refs/remotes/origin/master",
            "refs/heads/master",
        ],
    );
    git(dir, &["notes", "add", "-m", "a note", "refs/heads/master"]);
}

fn git_tree(dir: &Path, rev: &str) -> BTreeMap<String, (String, String)> {
    // path → (mode, git blob sha); -z avoids any quoting ambiguity
    let raw = git_out(dir, &["ls-tree", "-r", "-z", rev]);
    let mut out = BTreeMap::new();
    for rec in raw.split('\0') {
        if rec.is_empty() {
            continue;
        }
        // "<mode> <type> <sha>\t<path>"
        let (meta, path) = rec.split_once('\t').unwrap();
        let f: Vec<&str> = meta.split_whitespace().collect();
        out.insert(path.to_string(), (f[0].to_string(), f[2].to_string()));
    }
    out
}

fn git_blob(dir: &Path, sha: &str) -> Vec<u8> {
    git(dir, &["cat-file", "blob", sha]).stdout
}

fn git_commit_message(dir: &Path, rev: &str) -> Vec<u8> {
    let raw = git(dir, &["cat-file", "commit", rev]).stdout;
    let body = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .expect("Git commit header/body separator")
        + 2;
    raw[body..].to_vec()
}

fn mode_str(m: EntryMode) -> &'static str {
    match m {
        EntryMode::File => "100644",
        EntryMode::Executable => "100755",
        EntryMode::Symlink => "120000",
        EntryMode::Tree => "040000",
    }
}

fn ng_tree(repo: &Repo, snap: ObjectId) -> BTreeMap<String, (String, Vec<u8>)> {
    let s = match repo.objects.get(&snap).unwrap() {
        Object::Snapshot(s) => s,
        _ => panic!("not a snapshot"),
    };
    let mut out = BTreeMap::new();
    for (path, mode, oid) in flatten_tree(repo, s.root).unwrap() {
        let data = match repo.objects.get(&oid).unwrap() {
            Object::Blob(b) => b,
            _ => panic!("not a blob"),
        };
        out.insert(path, (mode_str(mode).to_string(), data));
    }
    out
}

/// Map git sha → newgit snapshot oid via extras.git_sha1, for every ref tip
/// and its ancestors.
fn snapshots_by_git_sha(repo: &Repo) -> BTreeMap<String, ObjectId> {
    let mut out = BTreeMap::new();
    let mut names = Vec::new();
    newgit::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
    for n in names {
        if n.starts_with("workspaces/") || n.starts_with("chains/") {
            continue;
        }
        let Some(tip) = repo.refs.read_opt(&n).unwrap() else {
            continue;
        };
        let mut stack = vec![tip];
        let mut seen = std::collections::HashSet::new();
        while let Some(o) = stack.pop() {
            if !seen.insert(o) {
                continue;
            }
            if let Ok(Object::Snapshot(s)) = repo.objects.get(&o) {
                if let Some(sha) = s.extras.get("git_sha1") {
                    out.insert(sha.clone(), o);
                }
                stack.extend(s.parents.iter().copied());
            }
        }
    }
    out
}

// ─────────────────────────── tests ───────────────────────────

#[test]
fn import_matches_git_content_exactly() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    rich_git_repo(&gdir);

    let (_nd, repo) = temp_repo();
    let rep = import_git(&repo, &gdir).unwrap();
    assert!(rep.commits >= 6, "{rep:?}");
    // refs imported
    for r in [
        "refs/heads/master",
        "refs/heads/feature",
        "refs/tags/light-tag",
        "refs/tags/v1.0",
    ] {
        assert!(
            repo.refs.read_opt(r).unwrap().is_some(),
            "missing imported ref {r}"
        );
    }
    // git-internal namespaces skipped and reported
    assert!(rep
        .refs_skipped
        .iter()
        .any(|s| s.starts_with("refs/remotes/")));
    assert!(rep
        .refs_skipped
        .iter()
        .any(|s| s.starts_with("refs/notes/")));
    assert!(repo
        .refs
        .read_opt("refs/remotes/origin/master")
        .unwrap()
        .is_none());
    // annotated tag reported as stripped, ref still imported
    assert!(rep
        .annotated_tags_stripped
        .iter()
        .any(|t| t == "refs/tags/v1.0"));
    // HEAD mirrored
    assert_eq!(rep.head.as_deref(), Some("ref: refs/heads/master"));
    match repo.read_head().unwrap() {
        newgit::repo::Head::Symbolic(n) => assert_eq!(n, "refs/heads/master"),
        _ => panic!("HEAD should be symbolic"),
    }
    // integrity
    let v = verify(&repo, &VerifyOpts { deep: true });
    assert!(v.ok(), "{:?}", v.issues);

    // ── per-commit tree/message/author/timestamp/tz equality vs git ──
    let by_sha = snapshots_by_git_sha(&repo);
    let all = git_out(&gdir, &["rev-list", "--branches", "--tags"]);
    for sha in all.lines() {
        let snap_oid = *by_sha
            .get(sha)
            .unwrap_or_else(|| panic!("git commit {sha} not imported"));
        let snap = match repo.objects.get(&snap_oid).unwrap() {
            Object::Snapshot(s) => s,
            _ => panic!(),
        };
        // tree equality (modes + paths + BYTES)
        let gt = git_tree(&gdir, sha);
        let nt = ng_tree(&repo, snap_oid);
        assert_eq!(gt.len(), nt.len(), "tree size mismatch for {sha}");
        for (path, (gmode, gsha)) in &gt {
            let (nmode, nbytes) = nt
                .get(path)
                .unwrap_or_else(|| panic!("path {path:?} missing in newgit tree of {sha}"));
            assert_eq!(gmode, nmode, "mode mismatch {path} @ {sha}");
            assert_eq!(
                &git_blob(&gdir, gsha)[..],
                &nbytes[..],
                "bytes mismatch {path} @ {sha}"
            );
        }
        // message (git's %B framing adds a record-terminating newline;
        // compare content, not framing)
        let gmsg = git_out(&gdir, &["log", "-1", "--format=%B", sha]);
        assert_eq!(
            snap.message.trim_end(),
            gmsg.trim_end(),
            "message mismatch for {sha}"
        );
        // author identity
        let gan = git_out(&gdir, &["log", "-1", "--format=%an", sha]);
        let gae = git_out(&gdir, &["log", "-1", "--format=%ae", sha]);
        let actor = match repo.objects.get(&snap.author).unwrap() {
            Object::Actor(a) => a,
            _ => panic!(),
        };
        assert_eq!(actor.display_name, gan.trim_end());
        assert_eq!(
            actor.extras.get("email").map(String::as_str),
            Some(gae.trim_end())
        );
        // timestamp (whole seconds) + tz
        let gts: i64 = git_out(&gdir, &["log", "-1", "--format=%at", sha])
            .trim()
            .parse()
            .unwrap();
        assert_eq!(snap.timestamp_ms / 1000, gts, "ts mismatch for {sha}");
        let gtz = git_out(
            &gdir,
            &["log", "-1", "--date=format:%z", "--format=%ad", sha],
        );
        let gtz = gtz.trim();
        let sign = if gtz.starts_with('-') { -1i32 } else { 1 };
        let hh: i32 = gtz[1..3].parse().unwrap();
        let mm: i32 = gtz[3..5].parse().unwrap();
        assert_eq!(
            snap.tz_offset_min as i32,
            sign * (hh * 60 + mm),
            "tz mismatch for {sha}"
        );
        // parents count matches
        let gparents = git_out(&gdir, &["log", "-1", "--format=%P", sha]);
        let gn = gparents.split_whitespace().count();
        assert_eq!(snap.parents.len(), gn, "parent count mismatch for {sha}");
    }
    // merge first-parent order preserved in extras
    let merge_sha = git_out(
        &gdir,
        &["rev-list", "--merges", "-n", "1", "refs/heads/master"],
    );
    let merge_sha = merge_sha.trim();
    let snap_oid = by_sha[merge_sha];
    let snap = match repo.objects.get(&snap_oid).unwrap() {
        Object::Snapshot(s) => s,
        _ => panic!(),
    };
    let ordered = snap
        .extras
        .get("git_parents_ordered")
        .expect("merge order extra");
    let gparents_out = git_out(&gdir, &["log", "-1", "--format=%P", merge_sha]);
    let gparents: Vec<&str> = gparents_out.split_whitespace().collect();
    let mapped: Vec<String> = gparents.iter().map(|s| by_sha[*s].to_hex()).collect();
    let ordered_v: Vec<&str> = ordered.split_whitespace().collect();
    assert_eq!(ordered_v, mapped);
}

#[test]
fn import_is_deterministic() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    rich_git_repo(&gdir);
    let (_d1, r1) = temp_repo();
    let (_d2, r2) = temp_repo();
    let a = import_git(&r1, &gdir).unwrap();
    let b = import_git(&r2, &gdir).unwrap();
    assert_eq!(a.refs_imported, b.refs_imported, "same repo ⇒ same oids");
    assert_eq!(a.commits, b.commits);
    assert_eq!(a.blobs, b.blobs);
}

#[test]
fn export_roundtrip_matches_git() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    rich_git_repo(&gdir);
    let (_nd, repo) = temp_repo();
    import_git(&repo, &gdir).unwrap();

    let outdir = d.path().join("out");
    let rep = export_git(&repo, &outdir).unwrap();
    assert!(rep.commits >= 6);

    // same commit count and ref set (annotated tag comes back lightweight)
    let n_orig: usize = git_out(&gdir, &["rev-list", "--branches", "--tags"])
        .lines()
        .count();
    let n_out: usize = git_out(&outdir, &["rev-list", "--branches", "--tags"])
        .lines()
        .count();
    assert_eq!(n_orig, n_out, "commit count changed in roundtrip");

    for r in [
        "refs/heads/master",
        "refs/heads/feature",
        "refs/tags/light-tag",
        "refs/tags/v1.0",
    ] {
        let t_orig = git_tree(&gdir, r);
        let t_out = git_tree(&outdir, r);
        assert_eq!(t_orig.len(), t_out.len(), "tree size for {r}");
        for (path, (mode, sha)) in &t_orig {
            let (mode2, sha2) = t_out
                .get(path)
                .unwrap_or_else(|| panic!("{path} missing in {r}"));
            assert_eq!(mode, mode2, "mode {path} @ {r}");
            assert_eq!(
                sha, sha2,
                "BLOB SHA {path} @ {r} — content must be byte-identical"
            );
        }
    }
    // author AND committer metadata identical as multisets (committer is
    // carried through extras.git_committer_* when it differs from author)
    let mut meta_orig: Vec<String> = git_out(
        &gdir,
        &[
            "log",
            "--branches",
            "--tags",
            "--format=%an|%ae|%cn|%ce|%at|%ct|%s",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect();
    let mut meta_out: Vec<String> = git_out(
        &outdir,
        &[
            "log",
            "--branches",
            "--tags",
            "--format=%an|%ae|%cn|%ce|%at|%ct|%s",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect();
    meta_orig.sort();
    meta_out.sort();
    assert_eq!(
        meta_orig, meta_out,
        "identity/timestamp metadata changed in roundtrip"
    );
    // messages identical as multisets
    let mut m_orig: Vec<String> =
        git_out(&gdir, &["log", "--branches", "--tags", "--format=%B%x1e"])
            .split('\x1e')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    let mut m_out: Vec<String> =
        git_out(&outdir, &["log", "--branches", "--tags", "--format=%B%x1e"])
            .split('\x1e')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    m_orig.sort();
    m_out.sort();
    assert_eq!(m_orig, m_out);
    // first-parent history of master preserved
    let fp_orig = git_out(
        &gdir,
        &["log", "--first-parent", "--format=%s", "refs/heads/master"],
    );
    let fp_out = git_out(
        &outdir,
        &["log", "--first-parent", "--format=%s", "refs/heads/master"],
    );
    assert_eq!(fp_orig, fp_out, "first-parent lineage changed");
    // working tree materialized and clean
    assert!(outdir.join("merged-note.txt").exists());
    let st = git_out(&outdir, &["status", "--porcelain"]);
    assert!(st.is_empty(), "exported worktree not clean: {st}");
    assert_eq!(
        git_out(&outdir, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/master"
    );
    // symlink + exec bit survived on disk
    assert!(outdir
        .join("link.txt")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn quoted_utf8_git_paths_roundtrip_without_changing_names() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    let first_path = "café \"quoted\"\\name.txt";
    write(&gdir, first_path, b"first content\n");
    commit(&gdir, "quoted UTF-8 path root");

    let second_path = "quoted \"résumé\"\\file.txt";
    git(&gdir, &["mv", first_path, second_path]);
    write(&gdir, second_path, b"renamed content\n");
    commit(&gdir, "rename quoted UTF-8 path");

    let fast_export = git(
        &gdir,
        &["fast-export", "--all", "--full-tree", "--show-original-ids"],
    )
    .stdout;
    let fast_export = String::from_utf8(fast_export).expect("fast-export is textual");
    assert!(
        fast_export.contains("\\303\\251"),
        "UTF-8 path was not C-quoted"
    );
    assert!(fast_export.contains("\\\""), "quote was not C-escaped");
    assert!(fast_export.contains("\\\\"), "backslash was not C-escaped");

    let (_nd, repo) = temp_repo();
    let imported = import_git(&repo, &gdir).unwrap();
    assert_eq!(imported.commits, 2);
    let outdir = d.path().join("out");
    let exported = export_git(&repo, &outdir).unwrap();
    assert_eq!(exported.commits, 2);

    let source_revs: Vec<String> =
        git_out(&gdir, &["rev-list", "--reverse", "--first-parent", "HEAD"])
            .lines()
            .map(str::to_string)
            .collect();
    let output_revs: Vec<String> = git_out(
        &outdir,
        &["rev-list", "--reverse", "--first-parent", "HEAD"],
    )
    .lines()
    .map(str::to_string)
    .collect();
    assert_eq!(source_revs.len(), 2);
    assert_eq!(output_revs.len(), source_revs.len());
    let imported_by_sha = snapshots_by_git_sha(&repo);
    for (source, output) in source_revs.iter().zip(&output_revs) {
        let snapshot_oid = imported_by_sha
            .get(source)
            .unwrap_or_else(|| panic!("source commit {source} missing after import"));
        let imported_tree = ng_tree(&repo, *snapshot_oid);
        let source_tree = git_tree(&gdir, source);
        assert_eq!(imported_tree.len(), source_tree.len());
        for (path, (mode, sha)) in &source_tree {
            let (imported_mode, imported_bytes) = imported_tree
                .get(path)
                .unwrap_or_else(|| panic!("imported path {path:?} missing at {source}"));
            assert_eq!(imported_mode, mode, "imported mode changed for {path:?}");
            assert_eq!(
                git_blob(&gdir, sha),
                *imported_bytes,
                "imported content changed for {path:?}"
            );
        }
        assert_eq!(
            source_tree,
            git_tree(&outdir, output),
            "quoted UTF-8 path, mode, or blob id changed for {source}"
        );
    }
    assert!(git_out(&outdir, &["status", "--porcelain"]).is_empty());
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
}

#[test]
fn commit_message_roundtrip_preserves_exact_utf8_bytes() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "seed.txt", b"seed\n");
    commit(&gdir, "seed");

    let expected = vec![
        b"seed\n".to_vec(),
        "\n\nleading blank lines; Unicode: λ\nbody with trailing spaces  \n\n"
            .as_bytes()
            .to_vec(),
        b"no final newline, but trailing spaces  ".to_vec(),
        b"CRLF line one\r\nline two\r\n".to_vec(),
        Vec::new(),
    ];
    for (index, message) in expected.iter().skip(1).enumerate() {
        let message_file = d.path().join(format!("message-{index}"));
        std::fs::write(&message_file, message).unwrap();
        git(
            &gdir,
            &[
                "-c",
                "commit.cleanup=verbatim",
                "commit",
                "--quiet",
                "--allow-empty",
                "--allow-empty-message",
                "-F",
                message_file.to_str().unwrap(),
            ],
        );
    }

    let source_revs: Vec<String> =
        git_out(&gdir, &["rev-list", "--reverse", "--first-parent", "HEAD"])
            .lines()
            .map(str::to_string)
            .collect();
    assert_eq!(source_revs.len(), expected.len());
    for (rev, message) in source_revs.iter().zip(&expected) {
        assert_eq!(
            git_commit_message(&gdir, rev),
            *message,
            "source Git fixture"
        );
    }

    let (_nd, repo) = temp_repo();
    let report = import_git(&repo, &gdir).unwrap();
    assert_eq!(report.commits, expected.len());
    let by_sha = snapshots_by_git_sha(&repo);
    for (rev, message) in source_revs.iter().zip(&expected) {
        let snapshot_oid = by_sha.get(rev).expect("imported source commit");
        let snapshot = match repo.objects.get(snapshot_oid).unwrap() {
            Object::Snapshot(snapshot) => snapshot,
            _ => panic!("imported commit did not map to a snapshot"),
        };
        assert_eq!(snapshot.message.as_bytes(), message, "imported {rev}");
    }

    let outdir = d.path().join("out");
    let export = export_git(&repo, &outdir).unwrap();
    assert_eq!(export.commits, expected.len());
    let output_revs: Vec<String> = git_out(
        &outdir,
        &["rev-list", "--reverse", "--first-parent", "HEAD"],
    )
    .lines()
    .map(str::to_string)
    .collect();
    assert_eq!(output_revs.len(), expected.len());
    for (rev, message) in output_revs.iter().zip(&expected) {
        assert_eq!(git_commit_message(&outdir, rev), *message, "exported {rev}");
    }
}

#[test]
fn git_control_character_commit_message_is_refused_atomically() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "seed.txt", b"seed\n");
    commit(&gdir, "seed");

    let message = b"valid UTF-8 with an unsupported control: \x01\n";
    let message_file = d.path().join("control-message");
    std::fs::write(&message_file, message).unwrap();
    git(
        &gdir,
        &[
            "-c",
            "commit.cleanup=verbatim",
            "commit",
            "--quiet",
            "--allow-empty",
            "-F",
            message_file.to_str().unwrap(),
        ],
    );
    let git_tip = git_out(&gdir, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(git_commit_message(&gdir, &git_tip), message);

    let (_nd, repo) = temp_repo();
    let error = import_git(&repo, &gdir).unwrap_err().to_string();
    assert!(error.contains(&git_tip), "missing commit id: {error}");
    assert!(error.contains("U+0001"), "missing code point: {error}");
    assert!(
        error.contains("before updating refs"),
        "not explicit: {error}"
    );

    let mut names = Vec::new();
    newgit::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
    assert!(names.is_empty(), "failed import moved refs: {names:?}");
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
}

#[test]
fn export_native_newgit_repo() {
    // A repo that never touched git: refs/main + a second ref.
    let (_nd, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    let root = repo.root().to_path_buf();
    write(&root, "x.txt", b"native\n");
    let _s1 = newgit::ops::snapshot::snapshot(
        &repo,
        &snapshot_req("main", "native one", author, 1_600_000_000_000),
    )
    .unwrap();
    write(&root, "y.txt", b"more\n");
    let _s2 = newgit::ops::snapshot::snapshot(
        &repo,
        &snapshot_req("main", "native two", author, 1_600_000_060_000),
    )
    .unwrap();

    let d = tempfile::tempdir().unwrap();
    let outdir = d.path().join("out");
    let rep = export_git(&repo, &outdir).unwrap();
    assert_eq!(rep.commits, 2);
    // refs/main → refs/heads/main
    assert!(rep
        .refs_exported
        .iter()
        .any(|(n, g)| n == "refs/main" && g == "refs/heads/main"));
    let log = git_out(&outdir, &["log", "--format=%s", "refs/heads/main"]);
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["native two", "native one"]
    );
    // author identity came from the actor object
    let an = git_out(&outdir, &["log", "-1", "--format=%an"]);
    assert!(!an.trim().is_empty());
    // timestamps preserved (whole seconds)
    let at: i64 = git_out(&outdir, &["log", "-1", "--format=%at"])
        .trim()
        .parse()
        .unwrap();
    assert_eq!(at, 1_600_000_060);
    // worktree usable
    assert_eq!(std::fs::read(outdir.join("y.txt")).unwrap(), b"more\n");
    let v = verify(&repo, &VerifyOpts { deep: true });
    assert!(v.ok());
}

#[test]
fn detached_head_only_history_roundtrips_without_pseudo_refs() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "only.txt", b"detached-only\n");
    commit(&gdir, "detached-only root");
    let git_tip = git_out(&gdir, &["rev-parse", "HEAD"]).trim().to_string();
    git(&gdir, &["checkout", "--quiet", "--detach", "HEAD"]);
    git(&gdir, &["branch", "-D", "master"]);
    assert!(git_out(&gdir, &["for-each-ref", "--format=%(refname)"]).is_empty());

    let (_nd, repo) = temp_repo();
    let rep = import_git(&repo, &gdir).unwrap();
    assert!(rep.refs_imported.is_empty(), "pseudo HEAD leaked: {rep:?}");
    assert!(repo.refs.list(None).unwrap().is_empty());
    let detached_oid = match repo.read_head().unwrap() {
        newgit::repo::Head::Detached(oid) => oid,
        other => panic!("expected detached HEAD, got {other:?}"),
    };
    let snap = match repo.objects.get(&detached_oid).unwrap() {
        Object::Snapshot(s) => s,
        _ => panic!("detached HEAD is not a snapshot"),
    };
    assert_eq!(
        snap.extras.get("git_sha1").map(String::as_str),
        Some(git_tip.as_str())
    );

    let outdir = d.path().join("out");
    export_git(&repo, &outdir).unwrap();
    assert_eq!(
        git_out(&outdir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert!(git_out(&outdir, &["for-each-ref", "--format=%(refname)"]).is_empty());
    assert_eq!(
        git(&outdir, &["show", "HEAD:only.txt"]).stdout,
        b"detached-only\n"
    );
    assert_eq!(
        git_out(&outdir, &["log", "-1", "--format=%s", "HEAD"]).trim(),
        "detached-only root"
    );
    assert!(git_out(&outdir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn detached_head_ahead_of_branch_roundtrips_without_moving_branch() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "base.txt", b"base\n");
    commit(&gdir, "branch base");
    let branch_tip = git_out(&gdir, &["rev-parse", "refs/heads/master"])
        .trim()
        .to_string();
    git(&gdir, &["checkout", "--quiet", "--detach", "HEAD"]);
    write(&gdir, "detached.txt", b"detached change\n");
    commit(&gdir, "detached successor");

    let (_nd, repo) = temp_repo();
    let rep = import_git(&repo, &gdir).unwrap();
    assert_eq!(rep.refs_imported.len(), 1, "pseudo HEAD leaked: {rep:?}");
    assert_eq!(rep.refs_imported[0].0, "refs/heads/master");
    assert!(repo.refs.read_opt("HEAD").unwrap().is_none());
    let detached_oid = match repo.read_head().unwrap() {
        newgit::repo::Head::Detached(oid) => oid,
        other => panic!("expected detached HEAD, got {other:?}"),
    };
    let snap = match repo.objects.get(&detached_oid).unwrap() {
        Object::Snapshot(s) => s,
        _ => panic!("detached HEAD is not a snapshot"),
    };
    assert_ne!(
        snap.extras.get("git_sha1").map(String::as_str),
        Some(branch_tip.as_str())
    );

    let outdir = d.path().join("out");
    export_git(&repo, &outdir).unwrap();
    assert_eq!(
        git_out(&outdir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(
        git_out(&outdir, &["log", "-1", "--format=%s", "refs/heads/master"]).trim(),
        "branch base"
    );
    assert_eq!(
        git_out(&outdir, &["log", "-1", "--format=%s", "HEAD"]).trim(),
        "detached successor"
    );
    assert_eq!(
        git(&outdir, &["show", "HEAD:detached.txt"]).stdout,
        b"detached change\n"
    );
    let refs = git_out(&outdir, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs.lines().collect::<Vec<_>>(), vec!["refs/heads/master"]);
    assert!(git_out(&outdir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn detached_head_export_avoids_nested_named_ref_collision() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "base.txt", b"base\n");
    commit(&gdir, "branch base");
    git(
        &gdir,
        &[
            "checkout",
            "--quiet",
            "-b",
            "newgit-export-detached-head/topic",
        ],
    );
    write(&gdir, "topic.txt", b"topic branch\n");
    commit(&gdir, "topic branch commit");
    git(&gdir, &["checkout", "--quiet", "master"]);
    git(
        &gdir,
        &[
            "checkout",
            "--quiet",
            "-b",
            "newgit-export-detached-head-1/topic",
        ],
    );
    write(&gdir, "topic-one.txt", b"second nested topic\n");
    commit(&gdir, "second nested topic commit");
    git(
        &gdir,
        &["checkout", "--quiet", "--detach", "refs/heads/master"],
    );
    write(&gdir, "detached.txt", b"detached successor\n");
    commit(&gdir, "detached successor");

    let (_nd, repo) = temp_repo();
    import_git(&repo, &gdir).unwrap();
    let outdir = d.path().join("out");
    export_git(&repo, &outdir).unwrap();

    assert_eq!(
        git_out(&outdir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
    assert_eq!(
        git(&outdir, &["show", "HEAD:detached.txt"]).stdout,
        b"detached successor\n"
    );
    assert_eq!(
        git(
            &outdir,
            &[
                "show",
                "refs/heads/newgit-export-detached-head/topic:topic.txt"
            ]
        )
        .stdout,
        b"topic branch\n"
    );
    assert_eq!(
        git(
            &outdir,
            &[
                "show",
                "refs/heads/newgit-export-detached-head-1/topic:topic-one.txt"
            ]
        )
        .stdout,
        b"second nested topic\n"
    );
    let refs = git_out(&outdir, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(
        refs.lines().collect::<Vec<_>>(),
        vec![
            "refs/heads/master",
            "refs/heads/newgit-export-detached-head-1/topic",
            "refs/heads/newgit-export-detached-head/topic"
        ]
    );
    assert!(git_out(&outdir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn submodule_import_is_refused_atomically() {
    let d = tempfile::tempdir().unwrap();
    // upstream repo to act as the submodule source
    let up = d.path().join("upstream");
    init_git(&up);
    write(&up, "lib.txt", b"lib\n");
    commit(&up, "upstream commit");

    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "main.txt", b"main\n");
    commit(&gdir, "base");
    git(
        &gdir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "../upstream",
            "sub",
        ],
    );
    commit(&gdir, "with submodule");

    let (_nd, repo) = temp_repo();
    let err = import_git(&repo, &gdir).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("submodule"), "error must say submodule: {msg}");
    // atomic: NO refs moved (the txn never ran)
    let mut names = Vec::new();
    newgit::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
    assert!(names.is_empty(), "refs must be untouched: {names:?}");
    // repo still healthy (orphan objects are gc fodder, not corruption)
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
}

#[test]
fn empty_git_repo_imports_cleanly() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir); // no commits
    let (_nd, repo) = temp_repo();
    let rep = import_git(&repo, &gdir).unwrap();
    assert_eq!(rep.commits, 0);
    assert!(rep.refs_imported.is_empty());
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
}

#[test]
fn import_into_used_repo_moves_refs_atomically() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "a.txt", b"a\n");
    commit(&gdir, "git side");

    let (_nd, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "own.txt", b"own\n");
    let own = newgit::ops::snapshot::snapshot(
        &repo,
        &snapshot_req("main", "own history", author, 1_500_000_000_000),
    )
    .unwrap();
    // park refs/heads/master on the native snapshot, then let the import
    // overwrite it (documented CAS::Any semantics)
    newgit::repo::txn::execute(
        repo.ng(),
        vec![newgit::repo::txn::TxnOp::Ref {
            name: "refs/heads/master".into(),
            cas: newgit::repo::txn::Cas::Any,
            new: Some(own.oid),
            log: newgit::repo::txn::RefLogEntry::system("test"),
        }],
        repo.limits(),
    )
    .unwrap();
    // import overwrites refs/heads/master
    let rep = import_git(&repo, &gdir).unwrap();
    assert_eq!(rep.refs_imported.len(), 1);
    let now = repo.refs.read("refs/heads/master").unwrap();
    assert_ne!(now, own.oid);
    // old snapshot still exists (immutable; gc would keep it via reflog)
    assert!(repo.objects.get(&own.oid).is_ok());
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
}

#[test]
fn export_refuses_dirty_targets_and_non_snapshot_refs() {
    let (_nd, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "x.txt", b"x\n");
    newgit::ops::snapshot::snapshot(&repo, &snapshot_req("main", "s", author, 1_600_000_000_000))
        .unwrap();
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("occupied.txt"), b"nope").unwrap();
    let err = export_git(&repo, &out).unwrap_err();
    assert!(err.to_string().contains("not empty"), "{err}");

    // a ref pointing at a non-snapshot must be refused loudly
    let blob = repo.objects.put_blob(b"stray").unwrap();
    newgit::repo::txn::execute(
        repo.ng(),
        vec![newgit::repo::txn::TxnOp::Ref {
            name: "refs/stray".into(),
            cas: newgit::repo::txn::Cas::Any,
            new: Some(blob),
            log: newgit::repo::txn::RefLogEntry::system("test"),
        }],
        repo.limits(),
    )
    .unwrap();
    let out2 = d.path().join("out2");
    let err = export_git(&repo, &out2).unwrap_err();
    assert!(err.to_string().contains("only snapshot histories"), "{err}");
}

#[test]
fn export_refuses_colliding_git_ref_names_without_partial_output() {
    let (_nd, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "x.txt", b"same target\n");
    let snap = newgit::ops::snapshot::snapshot(
        &repo,
        &snapshot_req("main", "same target", author, 1_600_000_000_000),
    )
    .unwrap();
    // Both `refs/main` and `main` map to `refs/heads/main` on export.
    newgit::repo::txn::execute(
        repo.ng(),
        vec![newgit::repo::txn::TxnOp::Ref {
            name: "main".into(),
            cas: newgit::repo::txn::Cas::Any,
            new: Some(snap.oid),
            log: newgit::repo::txn::RefLogEntry::system("collision regression"),
        }],
        repo.limits(),
    )
    .unwrap();

    let d = tempfile::tempdir().unwrap();
    let git_dir = d.path().join("git-check");
    init_git(&git_dir);
    assert_eq!(
        newgit::gitio::export::map_ref_name("main"),
        "refs/heads/main"
    );
    assert_eq!(
        newgit::gitio::export::map_ref_name("refs/main"),
        "refs/heads/main"
    );
    git(&git_dir, &["check-ref-format", "refs/heads/main"]);
    let outdir = d.path().join("out");
    let err = export_git(&repo, &outdir).unwrap_err();
    assert!(err.to_string().contains("both map to Git ref"), "{err}");
    assert!(
        !outdir.exists(),
        "ref collision must be detected before git init"
    );
}

#[test]
fn merge_with_redundant_ancestor_parent_exports_in_topological_order() {
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    init_git(&gdir);
    write(&gdir, "history.txt", b"A\n");
    commit(&gdir, "commit A");
    let a = git_out(&gdir, &["rev-parse", "HEAD"]).trim().to_string();
    write(&gdir, "history.txt", b"B\n");
    commit(&gdir, "commit B");
    let b = git_out(&gdir, &["rev-parse", "HEAD"]).trim().to_string();
    let tree = git_out(&gdir, &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    git(&gdir, &["branch", "-m", "master", "z-history"]);

    // Git permits an unusual but valid merge whose second parent is already
    // an ancestor of its first parent.
    let merge = Command::new("git")
        .arg("-C")
        .arg(&gdir)
        .args([
            "commit-tree",
            &tree,
            "-p",
            &b,
            "-p",
            &a,
            "-m",
            "redundant parent",
        ])
        .output()
        .unwrap();
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let merge_oid = String::from_utf8_lossy(&merge.stdout).trim().to_string();
    git(&gdir, &["update-ref", "refs/heads/a-merge", &merge_oid]);

    let (_nd, repo) = temp_repo();
    import_git(&repo, &gdir).unwrap();
    let outdir = d.path().join("out");
    export_git(&repo, &outdir).unwrap();

    let output = git_out(
        &outdir,
        &["rev-list", "--parents", "-n", "1", "refs/heads/a-merge"],
    );
    let fields = output.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "expected two ordered parents: {output:?}");
    assert_eq!(
        git_out(&outdir, &["show", "-s", "--format=%s", fields[1]]).trim(),
        "commit B"
    );
    assert_eq!(
        git_out(&outdir, &["show", "-s", "--format=%s", fields[2]]).trim(),
        "commit A"
    );
    assert!(git_out(&outdir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn reimport_after_export_is_stable() {
    // git → newgit → git → newgit: the second newgit import must produce
    // identical snapshots to the first (round-trip fixpoint on content).
    let d = tempfile::tempdir().unwrap();
    let gdir = d.path().join("g");
    rich_git_repo(&gdir);
    let (_n1, r1) = temp_repo();
    import_git(&r1, &gdir).unwrap();
    let outdir = d.path().join("out");
    export_git(&r1, &outdir).unwrap();
    let (_n2, r2) = temp_repo();
    import_git(&r2, &outdir).unwrap();

    // compare tree contents per ref (git shas differ — committer ts differs —
    // so compare NEWGIT oids of trees/messages, not snapshot oids)
    for r in ["refs/heads/master", "refs/heads/feature"] {
        let a = r1.refs.read(r).unwrap();
        let b = r2.refs.read(r).unwrap();
        let sa = match r1.objects.get(&a).unwrap() {
            Object::Snapshot(s) => s,
            _ => panic!(),
        };
        let sb = match r2.objects.get(&b).unwrap() {
            Object::Snapshot(s) => s,
            _ => panic!(),
        };
        assert_eq!(sa.root, sb.root, "tree of {r} tip differs after reimport");
        assert_eq!(sa.message, sb.message);
        // full history lengths match
        let ha = newgit::ops::history::history(&r1, Some(a), 0).unwrap();
        let hb = newgit::ops::history::history(&r2, Some(b), 0).unwrap();
        assert_eq!(ha.len(), hb.len(), "history length for {r}");
    }
}

fn snapshot_req(
    ws: &str,
    msg: &str,
    author: ObjectId,
    ts: i64,
) -> newgit::ops::snapshot::SnapshotRequest {
    newgit::ops::snapshot::SnapshotRequest {
        workspace: ws.into(),
        message: msg.into(),
        author,
        timestamp_ms: Some(ts),
        tz_offset_min: 0,
        goal: None,
        change: None,
        extras: BTreeMap::new(),
    }
}
