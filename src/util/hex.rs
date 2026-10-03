//! Lowercase hex encoding/decoding. Hand-rolled to avoid a dependency;
//! fully covered by unit + property tests.

use crate::error::{Error, Result};

const HEX: &[u8; 16] = b"0123456789abcdef";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::Malformed(format!(
            "invalid hex character: {:?}",
            c as char
        ))),
    }
}

pub fn decode(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return Err(Error::Malformed("hex string has odd length".into()));
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Ok(out)
}

/// Decode exactly `N` bytes from a fixed-size hex string.
pub fn decode_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    if s.len() != N * 2 {
        return Err(Error::Malformed(format!(
            "expected {} hex characters, got {}",
            N * 2,
            s.len()
        )));
    }
    let v = decode(s)?;
    let mut arr = [0u8; N];
    arr.copy_from_slice(&v);
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for v in [vec![], vec![0u8], vec![0, 15, 16, 255], b"hello".to_vec()] {
            assert_eq!(decode(&encode(&v)).unwrap(), v);
        }
    }

    #[test]
    fn known_vectors() {
        assert_eq!(encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(decode("DEADBEEF").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode("abc").is_err());
        assert!(decode("zz").is_err());
        assert!(decode_array::<4>("deadbe").is_err());
    }
}
