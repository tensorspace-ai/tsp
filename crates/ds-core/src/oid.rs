//! Object identity. An `Oid` is a sha256 digest, which is simultaneously the
//! git-lfs object id, our cache key, and the identity a `ds.lock` records.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Length of a sha256 digest rendered as lowercase hex.
pub const OID_HEX_LEN: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OidError {
    #[error("oid must be {OID_HEX_LEN} hex characters, got {0}")]
    Length(usize),
    #[error("oid must be lowercase hex, found {0:?}")]
    NotHex(char),
}

/// A validated sha256 object id.
///
/// Stored as the hex string rather than `[u8; 32]` because nearly every
/// consumer (cache paths, pointer text, batch JSON) wants hex; keeping the
/// bytes would mean re-encoding on each use.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid(String);

impl Oid {
    /// Validates and wraps a hex digest. Rejects uppercase: the git-lfs spec
    /// fixes lowercase, and accepting both would let one object occupy two
    /// cache paths.
    pub fn new(hex: impl Into<String>) -> Result<Self, OidError> {
        let hex = hex.into();
        if hex.len() != OID_HEX_LEN {
            return Err(OidError::Length(hex.len()));
        }
        if let Some(c) = hex.chars().find(|c| !matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(OidError::NotHex(c));
        }
        Ok(Self(hex))
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Cache path fanout, `ab/cd/abcd...` with the full oid as the filename.
    ///
    /// This is git-lfs's local layout. Gitea's server-side `RelativePath` looks
    /// similar but names the file `oid[4:]`; do not use one where the other is
    /// expected.
    pub fn fanout(&self) -> (&str, &str, &str) {
        (&self.0[0..2], &self.0[2..4], &self.0)
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Renders the abbreviated form used in human-facing output.
impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({}…)", &self.0[..12])
    }
}

impl FromStr for Oid {
    type Err = OidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl Serialize for Oid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Oid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn accepts_lowercase_hex() {
        assert_eq!(Oid::new(VALID).unwrap().as_str(), VALID);
    }

    #[test]
    fn rejects_uppercase() {
        // Same object, uppercase: would otherwise land in a second cache path.
        assert_eq!(Oid::new(VALID.to_uppercase()), Err(OidError::NotHex('D')));
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!(Oid::new("abc"), Err(OidError::Length(3)));
    }

    #[test]
    fn fanout_splits_and_keeps_full_oid_as_filename() {
        let oid = Oid::new(VALID).unwrap();
        assert_eq!(oid.fanout(), ("4d", "7a", VALID));
    }

    #[test]
    fn from_bytes_round_trips() {
        let bytes = [0xabu8; 32];
        let oid = Oid::from_bytes(&bytes);
        assert_eq!(oid.as_str(), "ab".repeat(32));
    }
}
