//! Canonical variable-length integers.
//!
//! Unsigned integers use canonical LEB128: little-endian base-128 groups of
//! 7 bits, with a continuation bit (0x80) on every byte except the last.
//!
//! Canonical form requirements (violation => reject):
//!   1. at most 10 bytes;
//!   2. value <= 2^64 - 1 (the 10th byte carries at most 1 payload bit);
//!   3. minimal form: if the encoding is longer than one byte, the final byte
//!      must be non-zero (no redundant leading zero groups).
//!
//! Signed integers use the zigzag map over the canonical unsigned encoding:
//!   z(n) = (n << 1) ^ (n >> 63)          (64-bit)
//!   n    = (z >> 1) ^ -(z & 1)

use std::fmt;

/// Errors produced by canonical integer decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarintError {
    /// The input ended before the value was complete.
    UnexpectedEof,
    /// The encoded value exceeds 2^64 - 1.
    Overflow,
    /// The encoding is longer than 10 bytes.
    TooLong,
    /// The encoding is not minimal (redundant leading zero group).
    NonCanonical,
}

impl fmt::Display for VarintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VarintError::UnexpectedEof => write!(f, "unexpected end of input"),
            VarintError::Overflow => write!(f, "varint value overflows u64"),
            VarintError::TooLong => write!(f, "varint longer than 10 bytes"),
            VarintError::NonCanonical => write!(f, "non-canonical (non-minimal) varint"),
        }
    }
}

impl std::error::Error for VarintError {}

/// Appends the canonical unsigned encoding of `v` to `out`.
pub fn encode_u64(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            return;
        }
    }
}

/// Appends the canonical zigzag signed encoding of `v` to `out`.
pub fn encode_i64(v: i64, out: &mut Vec<u8>) {
    let z = ((v as u64) << 1) ^ ((v >> 63) as u64);
    encode_u64(z, out);
}

/// Decodes one canonical unsigned integer from `buf` starting at `*pos`,
/// advancing `*pos` past the value. Rejects non-canonical encodings.
pub fn decode_u64(buf: &[u8], pos: &mut usize) -> Result<u64, VarintError> {
    let start = *pos;
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        if shift > 63 {
            return Err(VarintError::TooLong);
        }
        let byte = *buf.get(*pos).ok_or(VarintError::UnexpectedEof)?;
        *pos += 1;
        let payload = (byte & 0x7f) as u64;
        if shift == 63 {
            // 10th byte: at most 1 payload bit.
            if payload > 1 {
                return Err(VarintError::Overflow);
            }
            result |= payload << 63;
        } else {
            result |= payload << shift;
        }
        if byte & 0x80 == 0 {
            let len = *pos - start;
            if len > 1 && byte == 0x00 {
                return Err(VarintError::NonCanonical);
            }
            return Ok(result);
        }
        shift += 7;
        if *pos - start >= 10 {
            return Err(VarintError::TooLong);
        }
    }
}

/// Decodes one canonical zigzag signed integer from `buf` starting at `*pos`.
pub fn decode_i64(buf: &[u8], pos: &mut usize) -> Result<i64, VarintError> {
    let z = decode_u64(buf, pos)?;
    Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: u64) {
        let mut buf = Vec::new();
        encode_u64(v, &mut buf);
        let mut pos = 0;
        assert_eq!(decode_u64(&buf, &mut pos).unwrap(), v);
        assert_eq!(pos, buf.len());
    }

    #[test]
    fn u64_roundtrips() {
        for v in [0u64, 1, 127, 128, 33188, u32::MAX as u64, u64::MAX] {
            roundtrip(v);
        }
        for v in (0..100_000u64).step_by(7919) {
            roundtrip(v);
        }
    }

    #[test]
    fn i64_roundtrips() {
        for v in [0i64, 1, -1, i64::MAX, i64::MIN, 12345, -12345] {
            let mut buf = Vec::new();
            encode_i64(v, &mut buf);
            let mut pos = 0;
            assert_eq!(decode_i64(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn documented_encodings() {
        let mut b = Vec::new();
        encode_u64(0, &mut b);
        assert_eq!(b, vec![0x00]);
        b.clear();
        encode_u64(127, &mut b);
        assert_eq!(b, vec![0x7f]);
        b.clear();
        encode_u64(128, &mut b);
        assert_eq!(b, vec![0x80, 0x01]);
        b.clear();
        encode_u64(33188, &mut b);
        assert_eq!(b, vec![0xa4, 0x83, 0x02]);
    }

    #[test]
    fn rejects_non_canonical() {
        // 128 encoded with a redundant leading zero group.
        let buf = [0x80u8, 0x00];
        let mut pos = 0;
        assert_eq!(
            decode_u64(&buf, &mut pos).unwrap_err(),
            VarintError::NonCanonical
        );
        // Redundant trailing zero group.
        let buf = [0x80u8, 0x80, 0x00];
        let mut pos = 0;
        assert_eq!(
            decode_u64(&buf, &mut pos).unwrap_err(),
            VarintError::NonCanonical
        );
    }

    #[test]
    fn rejects_overflow_and_truncation() {
        // 10th byte with payload bits beyond bit 63.
        let buf = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        let mut pos = 0;
        assert_eq!(
            decode_u64(&buf, &mut pos).unwrap_err(),
            VarintError::Overflow
        );
        // Truncated.
        let buf = [0x80u8];
        let mut pos = 0;
        assert_eq!(
            decode_u64(&buf, &mut pos).unwrap_err(),
            VarintError::UnexpectedEof
        );
        // 11 bytes.
        let buf = [0x80u8; 11];
        let mut pos = 0;
        assert_eq!(
            decode_u64(&buf, &mut pos).unwrap_err(),
            VarintError::TooLong
        );
    }

    #[test]
    fn zigzag_values() {
        let mut b = Vec::new();
        encode_i64(-1, &mut b);
        assert_eq!(b, vec![0x01]);
        b.clear();
        encode_i64(1, &mut b);
        assert_eq!(b, vec![0x02]);
        b.clear();
        encode_i64(-2, &mut b);
        assert_eq!(b, vec![0x03]);
    }
}
