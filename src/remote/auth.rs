//! Bearer-token auth for the remote server (v1).
//!
//! Tokens are high-entropy random strings (32 bytes from the OS CSPRNG).
//! At rest only their SHA-256 digests are stored (hex), so a leaked token
//! file does not leak usable credentials. Roles: read < write < admin.
//! Lookup is by digest map — the raw token never needs comparison, and the
//! digest preimage resistance makes the map lookup the authentication step.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::util::{base64, fsx, hex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Read,
    Write,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        match s {
            "read" => Ok(Role::Read),
            "write" => Ok(Role::Write),
            "admin" => Ok(Role::Admin),
            _ => Err(Error::Invalid(format!(
                "role must be read|write|admin, got {s:?}"
            ))),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Read => "read",
            Role::Write => "write",
            Role::Admin => "admin",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenEntry {
    /// Stable public identifier (audit logs, CLI management).
    pub id: String,
    /// Hex SHA-256 of the raw token. The raw token is never stored.
    pub sha256: String,
    pub role: Role,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenFile {
    pub tokens: Vec<TokenEntry>,
}

/// An authenticated caller.
#[derive(Clone, Debug)]
pub struct Principal {
    pub id: String,
    pub role: Role,
}

pub fn hash_token(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    hex::encode(&h.finalize())
}

/// Generate a 32-byte random token (base64url-ish standard alphabet) from the
/// platform OS CSPRNG. Failure is loud; never fall back to weak randomness.
pub fn generate_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| {
        Error::Auth(format!(
            "cannot generate a secure token: OS CSPRNG failed ({e}); \
             no token was generated"
        ))
    })?;
    if buf.iter().all(|b| *b == 0) {
        return Err(Error::Auth("OS CSPRNG returned all-zero bytes".into()));
    }
    Ok(base64::encode(&buf))
}

pub fn load(path: &Path) -> Result<TokenFile> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let tf: TokenFile = serde_json::from_slice(&bytes)
                .map_err(|e| Error::Config(format!("token file {}: {e}", path.display())))?;
            for t in &tf.tokens {
                if t.id.is_empty() || t.sha256.len() != 64 {
                    return Err(Error::Config(format!(
                        "token file {}: entry {:?} malformed (id empty or sha256 not 64 hex)",
                        path.display(),
                        t.id
                    )));
                }
            }
            Ok(tf)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TokenFile::default()),
        Err(e) => Err(Error::Io {
            path: Some(path.to_path_buf()),
            source: e,
        }),
    }
}

/// Atomic write + best-effort 0600 (credentials file).
pub fn save(path: &Path, tf: &TokenFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fsx::ensure_dir(parent)?;
    }
    let json = serde_json::to_vec_pretty(tf).map_err(|e| Error::Bug(e.to_string()))?;
    fsx::atomic_write(path, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

impl TokenFile {
    fn index(&self) -> HashMap<&str, &TokenEntry> {
        self.tokens.iter().map(|t| (t.sha256.as_str(), t)).collect()
    }

    pub fn add(&mut self, id: &str, raw_token: &str, role: Role) -> Result<()> {
        if id.is_empty() || id.contains(|c: char| c.is_whitespace() || c == ':') {
            return Err(Error::Invalid(format!(
                "token id {id:?} must be non-empty without whitespace or ':'"
            )));
        }
        if self.tokens.iter().any(|t| t.id == id) {
            return Err(Error::Invalid(format!("token id {id:?} already exists")));
        }
        let sha = hash_token(raw_token);
        if self.tokens.iter().any(|t| t.sha256 == sha) {
            return Err(Error::Invalid(
                "a token with this exact value already exists".into(),
            ));
        }
        self.tokens.push(TokenEntry {
            id: id.to_string(),
            sha256: sha,
            role,
        });
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<bool> {
        let before = self.tokens.len();
        self.tokens.retain(|t| t.id != id);
        Ok(self.tokens.len() != before)
    }

    /// Authenticate an `Authorization` header value.
    /// `None` header ⇒ `Ok(None)` (anonymous — caller decides policy);
    /// present but invalid ⇒ `Err(Error::Auth)` (never falls back to anon).
    pub fn authenticate(&self, header: Option<&str>) -> Result<Option<Principal>> {
        let Some(h) = header else { return Ok(None) };
        let token = h
            .strip_prefix("Bearer ")
            .or_else(|| h.strip_prefix("bearer "))
            .ok_or_else(|| Error::Auth("Authorization header must be `Bearer <token>`".into()))?
            .trim();
        if token.is_empty() {
            return Err(Error::Auth("empty bearer token".into()));
        }
        let sha = hash_token(token);
        match self.index().get(sha.as_str()) {
            Some(entry) => Ok(Some(Principal {
                id: entry.id.clone(),
                role: entry.role,
            })),
            None => Err(Error::Auth("invalid token".into())),
        }
    }
}

/// Authorization: does `principal` satisfy the required minimum role?
pub fn authorize(principal: Option<&Principal>, required: Role) -> bool {
    match principal {
        Some(p) => p.role >= required,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_token_roundtrips() {
        let tf = {
            let mut tf = TokenFile::default();
            tf.add("ci", "sekrit-token-value", Role::Write).unwrap();
            tf
        };
        assert_eq!(tf.tokens[0].sha256, hash_token("sekrit-token-value"));
        let p = tf
            .authenticate(Some("Bearer sekrit-token-value"))
            .unwrap()
            .unwrap();
        assert_eq!(p.id, "ci");
        assert_eq!(p.role, Role::Write);
        assert!(authorize(Some(&p), Role::Read));
        assert!(authorize(Some(&p), Role::Write));
        assert!(!authorize(Some(&p), Role::Admin));
    }

    #[test]
    fn auth_failures_are_loud_never_anonymous() {
        let mut tf = TokenFile::default();
        tf.add("a", "tok", Role::Read).unwrap();
        assert!(tf.authenticate(Some("Bearer wrong")).is_err());
        assert!(tf.authenticate(Some("Basic dXNlcjpwYXNz")).is_err());
        assert!(tf.authenticate(Some("Bearer ")).is_err());
        // absent header is anonymous (policy decides), present-but-bad is not
        assert!(tf.authenticate(None).unwrap().is_none());
        // duplicate ids and duplicate token values rejected
        assert!(tf.add("a", "other", Role::Read).is_err());
        assert!(tf.add("b", "tok", Role::Read).is_err());
        assert!(tf.add("bad id", "x", Role::Read).is_err());
    }

    #[test]
    fn save_load_roundtrip_and_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("ngauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.json");
        assert_eq!(load(&path).unwrap().tokens.len(), 0);
        let mut tf = TokenFile::default();
        tf.add("deploy", "T", Role::Admin).unwrap();
        save(&path, &tf).unwrap();
        let mut loaded = load(&path).unwrap();
        assert_eq!(loaded.tokens[0].id, "deploy");
        assert_eq!(loaded.tokens[0].role, Role::Admin);
        assert!(loaded.remove("deploy").unwrap());
        assert!(!loaded.remove("deploy").unwrap());
        // malformed file ⇒ Config error, not panic
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(load(&path), Err(Error::Config(_))));
        std::fs::write(
            &path,
            br#"{"tokens":[{"id":"","sha256":"x","role":"read"}]}"#,
        )
        .unwrap();
        assert!(matches!(load(&path), Err(Error::Config(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_tokens_are_unique_and_long() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_ne!(a, b);
        assert!(a.len() >= 40, "token too short: {a}");
    }
}
