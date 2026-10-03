//! LEB128-style unsigned varints and zigzag-signed varints.
//! Used for all canonical object encodings; must be byte-stable forever.

use crate::error::{Error, Result};

/// Maximum encoded length we accept (10 bytes covers u64).
pub const MAX_VARINT_LEN: usize = 10;

pub fn write_u64(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_i64(out: &mut Vec<u8>, v: i64) {
    // zigzag
    let z = ((v << 1) ^ (v >> 63)) as u64;
    write_u64(out, z);
}

/// Cursor-based reader that never panics on malformed input.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        if self.remaining() < 1 {
            return Err(Error::Malformed("unexpected end of input (u8)".into()));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::Malformed(format!("invalid bool byte {other}"))),
        }
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        // Guard against length-prefix bombs before slicing.
        if n > self.remaining() {
            return Err(Error::Malformed(format!(
                "length prefix {n} exceeds remaining input {}",
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.read_bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        for i in 0..MAX_VARINT_LEN {
            let byte = self.read_u8()?;
            let low = (byte & 0x7f) as u64;
            // Reject non-minimal encodings and overlong values so that
            // canonical form is unique (identity is deterministic).
            if shift == 63 && low > 1 {
                return Err(Error::Malformed("varint overflow".into()));
            }
            if low.checked_shl(shift).is_none() {
                return Err(Error::Malformed("varint overflow".into()));
            }
            result |= low << shift;
            if byte & 0x80 == 0 {
                // canonical minimality: last byte must not be a redundant zero
                // continuation unless value needs it
                if i > 0 && low == 0 {
                    return Err(Error::Malformed("non-minimal varint".into()));
                }
                return Ok(result);
            }
            shift += 7;
        }
        Err(Error::Malformed("varint too long".into()))
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        let z = self.read_u64()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }

    pub fn read_str(&mut self) -> Result<&'a str> {
        let n = self.read_u64()? as usize;
        let b = self.read_bytes(n)?;
        std::str::from_utf8(b).map_err(|e| Error::Malformed(format!("invalid utf-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: u64) {
        let mut buf = Vec::new();
        write_u64(&mut buf, v);
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u64().unwrap(), v);
        assert!(r.is_empty());
    }

    #[test]
    fn u64_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, u64::MAX / 2, u64::MAX] {
            rt(v);
        }
    }

    #[test]
    fn i64_roundtrip() {
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -300] {
            let mut buf = Vec::new();
            write_i64(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.read_i64().unwrap(), v);
        }
    }

    #[test]
    fn minimal_encoding() {
        let mut buf = Vec::new();
        write_u64(&mut buf, 0);
        assert_eq!(buf, vec![0]);
        // non-minimal: 0 encoded as 0x80 0x00
        let mut r = Reader::new(&[0x80, 0x00]);
        assert!(r.read_u64().is_err());
    }

    #[test]
    fn truncated_input_never_panics() {
        for len in 0..10 {
            let mut r = Reader::new(&[0xff; 10][..len]);
            let _ = r.read_u64();
        }
    }
}
