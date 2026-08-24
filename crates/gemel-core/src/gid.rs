//! Object identities (Gid).
//!
//! A Gid is the family code byte plus the 32-byte BLAKE3-256 digest of the
//! canonical envelope (OBJECT_MODEL.md §2–§3). Binary form: 33 bytes.
//! Textual form: `<family-short>.<64 lowercase hex>`.

use crate::family::Family;
use crate::hex;
use std::fmt;

/// A canonical object identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Gid {
    family: Family,
    digest: [u8; 32],
}

/// Errors produced when parsing a textual identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseGidError {
    /// No `.` separator.
    MissingSeparator,
    /// Unknown family short name.
    UnknownFamily,
    /// The digest is not exactly 64 lowercase hex characters.
    MalformedDigest,
}

impl fmt::Display for ParseGidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseGidError::MissingSeparator => write!(f, "missing '.' separator in identity"),
            ParseGidError::UnknownFamily => write!(f, "unknown family in identity"),
            ParseGidError::MalformedDigest => write!(f, "malformed digest in identity"),
        }
    }
}

impl std::error::Error for ParseGidError {}

impl Gid {
    /// Builds a Gid from a family and a 32-byte digest.
    pub const fn new(family: Family, digest: [u8; 32]) -> Gid {
        Gid { family, digest }
    }

    /// The object family.
    pub const fn family(self) -> Family {
        self.family
    }

    /// The 32-byte digest.
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// The binary form: family code byte followed by the 32-byte digest.
    pub fn to_bytes(self) -> [u8; 33] {
        let mut out = [0u8; 33];
        out[0] = self.family.code();
        out[1..].copy_from_slice(&self.digest);
        out
    }

    /// Parses the binary form; rejects unknown families.
    pub fn from_bytes(bytes: &[u8; 33]) -> Option<Gid> {
        let family = Family::from_code(bytes[0])?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[1..]);
        Some(Gid::new(family, digest))
    }
}

impl fmt::Display for Gid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.family.short(), hex::encode(&self.digest))
    }
}

impl std::str::FromStr for Gid {
    type Err = ParseGidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (short, digest_hex) = s.rsplit_once('.').ok_or(ParseGidError::MissingSeparator)?;
        let family = Family::parse_short(short).ok_or(ParseGidError::UnknownFamily)?;
        // Textual identities are strictly lowercase (OBJECT_MODEL.md §3.1).
        if !digest_hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(ParseGidError::MalformedDigest);
        }
        let digest = hex::decode(digest_hex).map_err(|_| ParseGidError::MalformedDigest)?;
        if digest.len() != 32 {
            return Err(ParseGidError::MalformedDigest);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Ok(Gid::new(family, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: [u8; 32] = [0u8; 32];

    #[test]
    fn textual_roundtrip() {
        let g = Gid::new(Family::Change, ZERO);
        let s = g.to_string();
        assert!(s.starts_with("change."));
        assert_eq!(s.len(), 7 + 64);
        assert_eq!(s.parse::<Gid>().unwrap(), g);
    }

    #[test]
    fn binary_roundtrip() {
        let g = Gid::new(Family::ContextManifest, ZERO);
        let b = g.to_bytes();
        assert_eq!(b[0], 0x13);
        assert_eq!(Gid::from_bytes(&b), Some(g));
    }

    #[test]
    fn rejects_bad_input() {
        assert!("change.zzzz".parse::<Gid>().is_err());
        assert!("change".parse::<Gid>().is_err());
        assert!("nope.0000".parse::<Gid>().is_err());
        assert!("change.0000".parse::<Gid>().is_err()); // too short
        assert!(
            "change.ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"
                .parse::<Gid>()
                .is_err()
        ); // uppercase hex rejected
        let bad = [0xffu8; 33];
        assert_eq!(Gid::from_bytes(&bad), None);
    }
}
