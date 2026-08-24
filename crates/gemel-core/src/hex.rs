//! Deterministic lowercase hex encoding.

/// Errors produced when decoding hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexError {
    /// The input has an odd number of characters.
    OddLength,
    /// A character is not a hex digit.
    InvalidChar,
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Encodes bytes as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decodes lowercase (or uppercase) hex into bytes.
pub fn decode(s: &str) -> Result<Vec<u8>, HexError> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(HexError::OddLength);
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i]).ok_or(HexError::InvalidChar)?;
        let lo = nibble(bytes[i + 1]).ok_or(HexError::InvalidChar)?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let bytes = [0x00u8, 0x0f, 0x10, 0xff, 0xab, 0xcd];
        let s = encode(&bytes);
        assert_eq!(s, "000f10ffabcd");
        assert_eq!(decode(&s).unwrap(), bytes);
    }

    #[test]
    fn rejects() {
        assert_eq!(decode("abc").unwrap_err(), HexError::OddLength);
        assert_eq!(decode("abxz").unwrap_err(), HexError::InvalidChar);
    }
}
