//! Repository configuration and resource limits.
//!
//! Stored as a simple `key = value` file (`.newgit/config`); no TOML
//! dependency, deterministic parse, unknown keys are an error (fail loudly
//! on config we do not understand).

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::util::fsx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Max size of a single blob (file content).
    pub max_blob_bytes: u64,
    /// Max canonical size of any object.
    pub max_object_bytes: u64,
    /// Max on-disk envelope size.
    pub max_stored_bytes: u64,
    /// Max entries in one tree.
    pub max_tree_entries: usize,
    /// Max bytes in one path component.
    pub max_path_component: usize,
    /// Max directory depth when walking a workspace.
    pub max_depth: usize,
    /// How long to wait for locks (ms).
    pub lock_wait_ms: u64,
    /// Grace period before abandoned object-store temporary files are swept (s).
    pub temp_file_grace_s: u64,
    /// Max objects transferred in one remote batch.
    pub max_batch_objects: usize,
    /// Max HTTP request body (bytes).
    pub max_request_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_blob_bytes: 256 << 20,   // 256 MiB
            max_object_bytes: 512 << 20, // 512 MiB
            max_stored_bytes: 512 << 20,
            max_tree_entries: 1 << 20,
            max_path_component: 255,
            max_depth: 64,
            lock_wait_ms: 10_000,
            temp_file_grace_s: 300,
            max_batch_objects: 4096,
            max_request_bytes: 512 << 20,
        }
    }
}

impl Limits {
    pub fn to_map(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("max_blob_bytes".into(), self.max_blob_bytes.to_string()),
            ("max_object_bytes".into(), self.max_object_bytes.to_string()),
            ("max_stored_bytes".into(), self.max_stored_bytes.to_string()),
            ("max_tree_entries".into(), self.max_tree_entries.to_string()),
            (
                "max_path_component".into(),
                self.max_path_component.to_string(),
            ),
            ("max_depth".into(), self.max_depth.to_string()),
            ("lock_wait_ms".into(), self.lock_wait_ms.to_string()),
            (
                "temp_file_grace_s".into(),
                self.temp_file_grace_s.to_string(),
            ),
            (
                "max_batch_objects".into(),
                self.max_batch_objects.to_string(),
            ),
            (
                "max_request_bytes".into(),
                self.max_request_bytes.to_string(),
            ),
        ])
    }

    pub fn from_map(m: &BTreeMap<String, String>) -> Result<Limits> {
        let mut l = Limits::default();
        let mut saw_temp_file_grace = false;
        for (k, v) in m {
            let n = v
                .parse::<u64>()
                .map_err(|_| Error::Config(format!("limit {k} is not a number: {v:?}")))?;
            match k.as_str() {
                "max_blob_bytes" => l.max_blob_bytes = n,
                "max_object_bytes" => l.max_object_bytes = n,
                "max_stored_bytes" => l.max_stored_bytes = n,
                "max_tree_entries" => l.max_tree_entries = n as usize,
                "max_path_component" => {
                    if n == 0 || n > 4096 {
                        return Err(Error::Config("max_path_component out of range".into()));
                    }
                    l.max_path_component = n as usize
                }
                "max_depth" => {
                    if n == 0 || n > 4096 {
                        return Err(Error::Config("max_depth out of range".into()));
                    }
                    l.max_depth = n as usize
                }
                "lock_wait_ms" => l.lock_wait_ms = n,
                "temp_file_grace_s" | "lock_stale_s" => {
                    if saw_temp_file_grace {
                        return Err(Error::Config(
                            "specify only one of temp_file_grace_s and legacy lock_stale_s".into(),
                        ));
                    }
                    l.temp_file_grace_s = n;
                    saw_temp_file_grace = true;
                }
                "max_batch_objects" => l.max_batch_objects = n as usize,
                "max_request_bytes" => l.max_request_bytes = n,
                other => return Err(Error::Config(format!("unknown config key {other:?}"))),
            }
        }
        Ok(l)
    }
}

/// Full repo config = limits + default actor + misc settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoConfig {
    pub limits: Limits,
    /// Default author identity used when none is given on the command line.
    pub default_actor_id: Option<String>,
    pub default_actor_name: Option<String>,
    /// Repository format version (bumped only for breaking layout changes).
    pub format_version: u32,
}

pub const CURRENT_FORMAT_VERSION: u32 = 1;

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig {
            limits: Limits::default(),
            default_actor_id: None,
            default_actor_name: None,
            format_version: CURRENT_FORMAT_VERSION,
        }
    }
}

impl RepoConfig {
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("# newgit config v1 — key = value lines; '#' comments\n");
        out.push_str(&format!("format_version = {}\n", self.format_version));
        for (k, v) in self.limits.to_map() {
            out.push_str(&format!("{k} = {v}\n"));
        }
        if let Some(id) = &self.default_actor_id {
            out.push_str(&format!("default_actor_id = {id}\n"));
        }
        if let Some(name) = &self.default_actor_name {
            out.push_str(&format!("default_actor_name = {name}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Result<RepoConfig> {
        let mut map = BTreeMap::new();
        let mut cfg = RepoConfig::default();
        let mut saw_version = false;
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "config line {} is not `key = value`: {line:?}",
                    lineno + 1
                ))
            })?;
            let k = k.trim();
            let v = v.trim();
            match k {
                "format_version" => {
                    let n: u32 = v
                        .parse()
                        .map_err(|_| Error::Config("format_version not a number".into()))?;
                    if n != CURRENT_FORMAT_VERSION {
                        return Err(Error::Config(format!(
                            "unsupported repository format version {n} (this build supports {CURRENT_FORMAT_VERSION})"
                        )));
                    }
                    cfg.format_version = n;
                    saw_version = true;
                }
                "default_actor_id" => cfg.default_actor_id = Some(v.to_string()),
                "default_actor_name" => cfg.default_actor_name = Some(v.to_string()),
                _ => {
                    if map.contains_key(k) {
                        return Err(Error::Config(format!("duplicate config key {k:?}")));
                    }
                    map.insert(k.to_string(), v.to_string());
                }
            }
        }
        if !saw_version {
            return Err(Error::Config("config is missing format_version".into()));
        }
        cfg.limits = Limits::from_map(&map)?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<RepoConfig> {
        let text = fsx::read_limited(path, 1 << 20)?;
        let s = String::from_utf8(text)
            .map_err(|_| Error::Config("config file is not valid utf-8".into()))?;
        RepoConfig::parse(&s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fsx::atomic_write(path, self.serialize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let c = RepoConfig {
            default_actor_id: Some("human:alice".into()),
            default_actor_name: Some("Alice".into()),
            ..RepoConfig::default()
        };
        let parsed = RepoConfig::parse(&c.serialize()).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn config_rejects_unknown_and_bad() {
        assert!(RepoConfig::parse("format_version = 1\nwat = 3\n").is_err());
        assert!(RepoConfig::parse("wat = 3\n").is_err());
        assert!(RepoConfig::parse("format_version = 99\n").is_err());
        assert!(RepoConfig::parse("format_version = x\n").is_err());
        assert!(RepoConfig::parse("format_version = 1\nmax_depth = 0\n").is_err());
    }

    #[test]
    fn legacy_lock_stale_key_migrates_to_temp_file_grace() {
        let parsed = RepoConfig::parse("format_version = 1\nlock_stale_s = 17\n").unwrap();
        assert_eq!(parsed.limits.temp_file_grace_s, 17);
        let serialized = parsed.serialize();
        assert!(serialized.contains("temp_file_grace_s = 17"));
        assert!(!serialized.contains("lock_stale_s ="));
        assert!(RepoConfig::parse(
            "format_version = 1\nlock_stale_s = 17\ntemp_file_grace_s = 18\n"
        )
        .is_err());
    }
}
