//! On-disk object envelope (documented protocol — see docs/STORAGE_FORMAT.md).
//!
//! ```text
//! offset  size  field
//! 0       4     magic "NGOB"
//! 4       1     format version (= 1)
//! 5       1     object type tag (see types.rs)
//! 6       8     raw_len: u64 LE — length of canonical bytes
//! 14      8     stored_len: u64 LE — length of compressed payload
//! 22      n     payload: zlib (RFC 1950) compressed canonical bytes,
//!                compression level 6 (deterministic per flate2 version)
//! 22+n    32    digest: SHA-256 of canonical bytes == object id
//! ```
//!
//! The envelope is self-verifying: a reader recomputes SHA-256 over the
//! decompressed payload and compares with both the trailing digest and the
//! requested object id. Decompression is bounded (bomb protection).

use std::io::Read;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::object::id::ObjectId;
use crate::object::types::{Object, ObjectType};

pub const MAGIC: &[u8; 4] = b"NGOB";
pub const FORMAT_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 22;
pub const DIGEST_LEN: usize = 32;
pub const COMPRESSION_LEVEL: u32 = 6;

/// Encode an object into its on-disk envelope.
pub fn encode(obj: &Object) -> Result<Vec<u8>> {
    let canonical = obj.canonical();
    let id = ObjectId::compute(&canonical);
    encode_raw(&canonical, id)
}

/// Encode canonical bytes with a precomputed id (avoids double hashing).
pub fn encode_raw(canonical: &[u8], id: ObjectId) -> Result<Vec<u8>> {
    if canonical.is_empty() {
        return Err(Error::Malformed("empty canonical object".into()));
    }
    let tag = ObjectType::from_u8(canonical[0])?;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(COMPRESSION_LEVEL));
    std::io::Write::write_all(&mut enc, canonical).map_err(|e| Error::Bug(e.to_string()))?;
    let compressed = enc.finish().map_err(|e| Error::Bug(e.to_string()))?;

    let mut out = Vec::with_capacity(HEADER_LEN + compressed.len() + DIGEST_LEN);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.push(tag as u8);
    out.extend_from_slice(&(canonical.len() as u64).to_le_bytes());
    out.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    out.extend_from_slice(&compressed);
    out.extend_from_slice(id.as_bytes());
    Ok(out)
}

/// Verify an envelope; return raw canonical bytes + trusted id, without
/// decoding the object semantics. `max_raw` bounds decompressed size
/// (decompression-bomb protection).
pub fn verify(bytes: &[u8], max_raw: u64) -> Result<(Vec<u8>, ObjectId)> {
    let (raw, digest) = verify_payload(bytes, max_raw)?;
    let id = ObjectId::from_bytes(digest);
    Ok((raw, id))
}

fn verify_payload(bytes: &[u8], max_raw: u64) -> Result<(Vec<u8>, [u8; 32])> {
    if bytes.len() < HEADER_LEN + DIGEST_LEN {
        return Err(Error::Malformed(format!(
            "envelope too short: {} bytes",
            bytes.len()
        )));
    }
    if &bytes[0..4] != MAGIC {
        return Err(Error::Malformed("bad envelope magic".into()));
    }
    let version = bytes[4];
    if version != FORMAT_VERSION {
        return Err(Error::Malformed(format!(
            "unsupported envelope version {version}"
        )));
    }
    // Validate the tag byte early (unknown tags are malformed envelopes).
    ObjectType::from_u8(bytes[5])?;
    let raw_len = u64::from_le_bytes(bytes[6..14].try_into().unwrap());
    let stored_len = u64::from_le_bytes(bytes[14..22].try_into().unwrap());
    let (payload, digest) = bytes[HEADER_LEN..].split_at(bytes.len() - HEADER_LEN - DIGEST_LEN);
    if payload.len() as u64 != stored_len {
        return Err(Error::Malformed(format!(
            "stored_len {stored_len} does not match payload {}",
            payload.len()
        )));
    }
    if raw_len > max_raw {
        return Err(Error::Limit(format!(
            "object raw size {raw_len} exceeds limit {max_raw}"
        )));
    }
    // Bounded decompression: never trust raw_len alone.
    let decoder = ZlibDecoder::new(payload);
    let mut raw = Vec::with_capacity(raw_len.min(1 << 20) as usize);
    decoder
        .take(max_raw.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|e| Error::Malformed(format!("zlib inflate failed: {e}")))?;
    if raw.len() as u64 != raw_len {
        return Err(Error::Malformed(format!(
            "decompressed {} bytes, envelope declared {raw_len}",
            raw.len()
        )));
    }
    let mut h = Sha256::new();
    h.update(&raw);
    let computed = h.finalize();
    if computed.as_slice() != digest {
        return Err(Error::Malformed(
            "digest mismatch: stored object does not match its envelope digest".into(),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(digest);
    Ok((raw, arr))
}

/// Decode and verify an envelope into a semantic object.
///
/// Guarantees on success:
/// * payload decompresses to exactly `raw_len` bytes within `max_raw`,
/// * SHA-256(payload) == trailing digest == returned id,
/// * payload parses as a canonical object of the declared type.
pub fn decode(bytes: &[u8], max_raw: u64) -> Result<(Object, ObjectId)> {
    let (raw, id) = verify(bytes, max_raw)?;
    // Safe: verify() succeeded, so bytes has a full header.
    let tag = ObjectType::from_u8(bytes[5])?;
    let obj = Object::from_canonical(&raw)?;
    if obj.type_tag() != tag {
        return Err(Error::Malformed(format!(
            "envelope declares type {} but payload is {}",
            tag.name(),
            obj.type_tag().name()
        )));
    }
    Ok((obj, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::types::Tree;

    #[test]
    fn encode_decode_roundtrip() {
        let obj = Object::Blob(b"some content".to_vec());
        let env = encode(&obj).unwrap();
        let (d, id) = decode(&env, 1 << 30).unwrap();
        assert_eq!(d, obj);
        assert_eq!(id, obj.id());
    }

    #[test]
    fn deterministic_encoding() {
        let obj = Object::Tree(Tree::empty());
        assert_eq!(encode(&obj).unwrap(), encode(&obj).unwrap());
    }

    #[test]
    fn detects_bit_flip() {
        let obj = Object::Blob(vec![7u8; 1000]);
        let mut env = encode(&obj).unwrap();
        // flip a bit in the compressed payload region
        let i = HEADER_LEN + 5;
        env[i] ^= 0x40;
        assert!(decode(&env, 1 << 30).is_err());
        // flip a bit in the raw (incompressible-ish) content of a small blob
        let obj2 = Object::Blob(b"hello world, this is a test".to_vec());
        let mut env2 = encode(&obj2).unwrap();
        let last = env2.len() - DIGEST_LEN - 1;
        env2[last] ^= 0x01;
        assert!(decode(&env2, 1 << 30).is_err());
    }

    #[test]
    fn bomb_protection() {
        // highly compressible 8 MiB blob, but max_raw = 1 KiB
        let obj = Object::Blob(vec![0u8; 8 << 20]);
        let env = encode(&obj).unwrap();
        assert!(matches!(decode(&env, 1024), Err(Error::Limit(_))));
        assert!(decode(&env, 16 << 20).is_ok());
    }

    #[test]
    fn truncated_envelopes_never_panic() {
        let obj = Object::Blob(b"x".repeat(5000));
        let env = encode(&obj).unwrap();
        for cut in 0..env.len() {
            let _ = decode(&env[..cut], 1 << 30);
        }
    }

    #[test]
    fn type_mismatch_detected() {
        let obj = Object::Blob(b"abc".to_vec());
        let mut env = encode(&obj).unwrap();
        env[5] = 2; // claim tree
        assert!(decode(&env, 1 << 30).is_err());
    }
}
