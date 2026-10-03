//! Object identity: SHA-256 over the canonical encoding of an object.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::util::hex;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub const LEN: usize = 32;
    pub const HEX_LEN: usize = 64;

    pub fn from_bytes(b: [u8; 32]) -> Self {
        ObjectId(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Identity is *defined* as SHA-256 over canonical bytes.
    pub fn compute(canonical: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(canonical);
        let d = h.finalize();
        let mut b = [0u8; 32];
        b.copy_from_slice(&d);
        ObjectId(b)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.len() != Self::HEX_LEN {
            return Err(Error::Malformed(format!(
                "object id must be {} hex chars, got {}",
                Self::HEX_LEN,
                s.len()
            )));
        }
        Ok(ObjectId(hex::decode_array::<32>(s)?))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    /// Short display form (first 12 hex chars).
    pub fn short(&self) -> String {
        self.to_hex()[..12].to_string()
    }

    pub fn starts_with_hex(&self, prefix: &str) -> bool {
        let p = prefix.to_ascii_lowercase();
        self.to_hex().starts_with(&p)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short())
    }
}

impl FromStr for ObjectId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        ObjectId::from_hex(s)
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ObjectId::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_identity() {
        let a = ObjectId::compute(b"hello");
        let b = ObjectId::compute(b"hello");
        let c = ObjectId::compute(b"hello!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(
            a.to_hex(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn hex_roundtrip() {
        let a = ObjectId::compute(b"x");
        assert_eq!(ObjectId::from_hex(&a.to_hex()).unwrap(), a);
        assert!(ObjectId::from_hex("nothex").is_err());
        assert!(ObjectId::from_hex(&"ab".repeat(31)).is_err());
    }
}
