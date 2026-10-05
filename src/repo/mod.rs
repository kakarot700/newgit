//! Repository layer: layout, config, object store, refs, transactions,
//! HEAD, and the actor registry.

pub mod config;
pub mod ignore;
pub mod index;
pub mod ostore;
pub mod refs;
pub mod txn;
pub mod walk;
pub mod workspace;

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::object::types::{Actor, ActorKind, Object};
use crate::object::ObjectId;
use crate::repo::config::{RepoConfig, CURRENT_FORMAT_VERSION};
use crate::repo::ostore::ObjectStore;
use crate::repo::refs::RefStore;
use crate::repo::txn::{RecoveryReport, RefLogEntry, TxnOp};
use crate::util::fsx;

pub use config::Limits;
pub use ostore::ObjectStore as Objects;

pub const NG_DIR: &str = ".newgit";
pub const DEFAULT_BRANCH: &str = "refs/main";

#[derive(Debug)]
pub struct Repo {
    root: PathBuf,
    ng: PathBuf,
    pub config: RepoConfig,
    pub objects: ObjectStore,
    pub refs: RefStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// Symbolic: points at a ref name (e.g. `refs/main`).
    Symbolic(String),
    /// Detached: points directly at a snapshot.
    Detached(ObjectId),
}

impl Repo {
    /// Initialize a repository at `root` (creates `<root>/.newgit`).
    /// Idempotent: existing config/HEAD are preserved.
    pub fn init(root: &Path) -> Result<Repo> {
        Repo::init_with(root, RepoConfig::default())
    }

    pub fn init_with(root: &Path, cfg: RepoConfig) -> Result<Repo> {
        let root = root.canonicalize().map_err(|e| Error::io(root, e))?;
        if !root.is_dir() {
            return Err(Error::Invalid(format!(
                "repository root is not a directory: {}",
                root.display()
            )));
        }
        let ng = root.join(NG_DIR);
        for d in [
            "objects",
            "refs",
            "logs/refs",
            "txn",
            "actors",
            "workspaces",
            "git-map",
        ] {
            fsx::ensure_dir(&ng.join(d))?;
        }
        let cfg_path = ng.join("config");
        if !cfg_path.exists() {
            cfg.save(&cfg_path)?;
        }
        let config = RepoConfig::load(&cfg_path)?;
        txn::initialize_head_if_missing(&ng, &config.limits)?;
        Repo::open_at(root, ng)
    }

    /// Open an existing repository whose root is exactly `root`.
    pub fn open(root: &Path) -> Result<Repo> {
        let root = root.canonicalize().map_err(|e| Error::io(root, e))?;
        let ng = root.join(NG_DIR);
        if !ng.is_dir() {
            return Err(Error::NotRepo(root));
        }
        Repo::open_at(root, ng)
    }

    /// Discover a repository by walking up from `start` (like git).
    pub fn discover(start: &Path) -> Result<Repo> {
        let start = start.canonicalize().map_err(|e| Error::io(start, e))?;
        let mut cur = start.as_path();
        loop {
            if cur.join(NG_DIR).is_dir() {
                return Repo::open(cur);
            }
            match cur.parent() {
                Some(p) => cur = p,
                None => return Err(Error::NotRepo(start)),
            }
        }
    }

    fn open_at(root: PathBuf, ng: PathBuf) -> Result<Repo> {
        let config = RepoConfig::load(&ng.join("config"))?;
        if config.format_version != CURRENT_FORMAT_VERSION {
            return Err(Error::Config(format!(
                "repository format {} != supported {}",
                config.format_version, CURRENT_FORMAT_VERSION
            )));
        }
        let objects = ObjectStore::new(ng.join("objects"), config.limits.clone());
        let refs = RefStore::new(ng.clone(), config.limits.clone());
        let repo = Repo {
            root,
            ng,
            config,
            objects,
            refs,
        };
        // Crash recovery on every open: redo incomplete transactions and
        // sweep stale object-store temp debris.
        repo.recover()?;
        Ok(repo)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn ng(&self) -> &Path {
        &self.ng
    }
    pub fn limits(&self) -> &Limits {
        &self.config.limits
    }

    /// Run recovery explicitly (also happens on open).
    pub fn recover(&self) -> Result<(RecoveryReport, usize)> {
        let r = txn::recover(&self.ng, &self.config.limits)?;
        let swept = self
            .objects
            .sweep_temp_files(self.config.limits.temp_file_grace_s)?;
        Ok((r, swept))
    }

    // ------------------------------------------------------------------
    // HEAD
    // ------------------------------------------------------------------

    pub fn read_head(&self) -> Result<Head> {
        let path = self.ng.join("HEAD");
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        let s =
            String::from_utf8(bytes).map_err(|_| Error::Malformed("HEAD is not utf-8".into()))?;
        let s = s.trim_end_matches('\n');
        if let Some(target) = s.strip_prefix("ref: ") {
            refs::check_ref_name(target)?;
            Ok(Head::Symbolic(target.to_string()))
        } else if s.len() == ObjectId::HEX_LEN {
            Ok(Head::Detached(ObjectId::from_hex(s)?))
        } else {
            Err(Error::Malformed(format!("unparsable HEAD: {s:?}")))
        }
    }

    /// Resolve HEAD to a snapshot id (None when unborn/missing).
    pub fn resolve_head(&self) -> Result<Option<ObjectId>> {
        match self.read_head()? {
            Head::Symbolic(name) => self.refs.read_opt(&name),
            Head::Detached(oid) => Ok(Some(oid)),
        }
    }

    pub fn set_head(&self, head: &Head, log: RefLogEntry) -> Result<()> {
        let content = match head {
            Head::Symbolic(name) => {
                refs::check_ref_name(name)?;
                format!("ref: {name}\n")
            }
            Head::Detached(oid) => format!("{}\n", oid.to_hex()),
        };
        // HEAD changes ride the same transaction machinery as refs.
        txn::execute(
            &self.ng,
            vec![TxnOp::File {
                rel: "HEAD".into(),
                data: content.into_bytes(),
            }],
            &self.config.limits,
        )?;
        // Symbolic HEAD creation is logged on the target ref when it is born;
        // detached moves are logged to logs/HEAD.
        let log_line = format!(
            "{} {} {}\n",
            match head {
                Head::Symbolic(n) => format!("ref {n}"),
                Head::Detached(o) => o.to_hex(),
            },
            log.ts_ms,
            log.message
        );
        let log_path = self.ng.join("logs/HEAD");
        fsx::ensure_dir(log_path.parent().unwrap())?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| Error::io(&log_path, e))?;
        f.write_all(log_line.as_bytes())
            .map_err(|e| Error::io(&log_path, e))?;
        f.sync_all().map_err(|e| Error::io(&log_path, e))?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Actor registry
    // ------------------------------------------------------------------

    fn actor_registry_path(&self, actor_id: &str) -> PathBuf {
        let mut h = Sha256::new();
        h.update(actor_id.as_bytes());
        let digest = crate::util::hex::encode(&h.finalize());
        self.ng.join("actors").join(digest)
    }

    /// Register (or re-register) an actor; returns its object id.
    /// The registry maps actor *id string* → latest actor object.
    pub fn register_actor(&self, actor: &Actor) -> Result<ObjectId> {
        actor.validate()?;
        let oid = self.objects.put(&Object::Actor(actor.clone()))?;
        fsx::atomic_write(
            &self.actor_registry_path(&actor.id),
            format!("{}\n", oid.to_hex()).as_bytes(),
        )?;
        Ok(oid)
    }

    /// Look up the current actor object for an actor id string.
    pub fn lookup_actor(&self, actor_id: &str) -> Result<Option<ObjectId>> {
        let path = self.actor_registry_path(actor_id);
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let s = s.trim();
                if s.len() != ObjectId::HEX_LEN {
                    return Err(Error::Malformed(format!(
                        "actor registry entry malformed: {}",
                        path.display()
                    )));
                }
                Ok(Some(ObjectId::from_hex(s)?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// The default actor for operations in this environment.
    ///
    /// Resolution order: repo config → `NEWGIT_ACTOR_ID`/`NEWGIT_ACTOR_NAME`
    /// env → `anonymous:local`. Kind is inferred from the id prefix
    /// (`human:`, `agent:`, `process:`); anything else is anonymous.
    pub fn default_actor(&self) -> Result<ObjectId> {
        let (id, name) = match (
            &self.config.default_actor_id,
            &self.config.default_actor_name,
        ) {
            (Some(id), Some(name)) => (id.clone(), name.clone()),
            (Some(id), None) => (id.clone(), id.clone()),
            _ => {
                let id = std::env::var("NEWGIT_ACTOR_ID")
                    .unwrap_or_else(|_| "anonymous:local".to_string());
                let name = std::env::var("NEWGIT_ACTOR_NAME").unwrap_or_else(|_| {
                    std::env::var("USER").unwrap_or_else(|_| "local".to_string())
                });
                (id, name)
            }
        };
        if let Some(oid) = self.lookup_actor(&id)? {
            // Refresh only if metadata is identical; otherwise register new.
            if let Ok(Object::Actor(existing)) = self.objects.get(&oid) {
                if existing.id == id && existing.display_name == name {
                    return Ok(oid);
                }
            }
        }
        let kind = match id.split_once(':') {
            Some(("human", _)) => ActorKind::Human,
            Some(("agent", _)) => ActorKind::Agent,
            Some(("process", _)) => ActorKind::Process,
            _ => ActorKind::Anonymous,
        };
        let actor = Actor {
            kind,
            id: id.clone(),
            display_name: name,
            tool: "newgit".into(),
            tool_version: crate::VERSION.into(),
            pubkey: None,
            extras: Default::default(),
        };
        self.register_actor(&actor)
    }

    /// Convenience: store an object.
    pub fn put(&self, obj: &Object) -> Result<ObjectId> {
        self.objects.put(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_open_discover() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        let repo = Repo::init(&root).unwrap();
        assert!(repo.ng.join("objects").is_dir());
        assert_eq!(
            repo.read_head().unwrap(),
            Head::Symbolic(DEFAULT_BRANCH.into())
        );
        assert_eq!(repo.resolve_head().unwrap(), None); // unborn
        drop(repo);
        // idempotent re-init
        let repo = Repo::init(&root).unwrap();
        assert_eq!(repo.config.format_version, CURRENT_FORMAT_VERSION);
        // open exact
        let repo2 = Repo::open(&root).unwrap();
        assert_eq!(repo2.root(), repo.root());
        // discover from subdir
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let repo3 = Repo::discover(&sub).unwrap();
        assert_eq!(repo3.root(), repo.root());
        // not a repo
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        assert!(matches!(Repo::open(&other), Err(Error::NotRepo(_))));
        assert!(matches!(Repo::discover(&other), Err(Error::NotRepo(_))));
    }

    #[test]
    fn head_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        let oid = ObjectId::from_bytes([3; 32]);
        repo.set_head(&Head::Detached(oid), RefLogEntry::system("test"))
            .unwrap();
        assert_eq!(repo.read_head().unwrap(), Head::Detached(oid));
        assert_eq!(repo.resolve_head().unwrap(), Some(oid));
        repo.set_head(
            &Head::Symbolic("refs/dev".into()),
            RefLogEntry::system("switch"),
        )
        .unwrap();
        assert_eq!(repo.read_head().unwrap(), Head::Symbolic("refs/dev".into()));
        assert_eq!(repo.resolve_head().unwrap(), None);
    }

    #[test]
    fn head_rejects_bad_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        assert!(repo
            .set_head(&Head::Symbolic("../evil".into()), RefLogEntry::system("x"))
            .is_err());
    }

    #[test]
    fn actor_registry() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        assert_eq!(repo.lookup_actor("human:alice").unwrap(), None);
        let a = Actor {
            kind: ActorKind::Human,
            id: "human:alice".into(),
            display_name: "Alice".into(),
            tool: "cli".into(),
            tool_version: "1".into(),
            pubkey: None,
            extras: Default::default(),
        };
        let oid = repo.register_actor(&a).unwrap();
        assert_eq!(repo.lookup_actor("human:alice").unwrap(), Some(oid));
        let fetched = repo.objects.get(&oid).unwrap();
        assert_eq!(fetched.as_actor().unwrap(), &a);
    }

    #[test]
    fn default_actor_env() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init(dir.path()).unwrap();
        // deterministic within test process (env mutation is process-wide;
        // tests using env vars are serialized by using distinct values)
        std::env::set_var("NEWGIT_ACTOR_ID", "agent:test-bot");
        std::env::set_var("NEWGIT_ACTOR_NAME", "Test Bot");
        let oid = repo.default_actor().unwrap();
        let actor = repo.objects.get(&oid).unwrap();
        let actor = actor.as_actor().unwrap();
        assert_eq!(actor.kind, ActorKind::Agent);
        assert_eq!(actor.id, "agent:test-bot");
        std::env::remove_var("NEWGIT_ACTOR_ID");
        std::env::remove_var("NEWGIT_ACTOR_NAME");
    }
}
