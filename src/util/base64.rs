//! Minimal standard-alphabet base64 (RFC 4648, with padding).
//! Used for journal/reflog text formats. Hand-rolled (dependency policy);
//! covered by unit + property tests.

use crate::error::{Error, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn val(c: u8) -> Result<u32> {
    match c {
        b'A'..=b'Z' => Ok((c - b'A') as u32),
        b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
        b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(Error::Malformed(format!(
            "invalid base64 character {:?}",
            c as char
        ))),
    }
}

pub fn decode(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 4 != 0 {
        return Err(Error::Malformed("base64 length not a multiple of 4".into()));
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let nchunks = b.len() / 4;
    for (ci, chunk) in b.chunks(4).enumerate() {
        let is_last = ci + 1 == nchunks;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return Err(Error::Malformed("invalid base64 padding".into()));
        }
        if pad > 0 && !is_last {
            return Err(Error::Malformed("base64 padding before end".into()));
        }
        let mut n: u32 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                if i < chunk.len() - pad {
                    return Err(Error::Malformed("misplaced base64 padding".into()));
                }
                continue;
            }
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode("").unwrap(), b"");
    }

    #[test]
    fn rejects_bad() {
        assert!(decode("Zm9").is_err());
        assert!(decode("Zm=v").is_err());
        assert!(decode("Zg==Zg==").is_err()); // padding only legal at the end
        assert!(decode("Z=== ").is_err());
        assert!(decode("*m9v").is_err());
        assert_eq!(decode("Zm9vYmFyZg==").unwrap(), b"foobarf");
    }

    #[test]
    fn roundtrip_all_bytes() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
    }
}
