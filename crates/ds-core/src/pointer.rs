//! The git-lfs pointer file format.
//!
//! spec: <https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md#the-pointer>
//!
//! `ds` reads these; git-lfs's clean filter writes them. Reading is what makes
//! locking cheap: a pointer already states the sha256 of the content it stands
//! for, so recording the identity of a multi-gigabyte output costs a blob read
//! rather than a pass over the data.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::oid::{Oid, OidError};

/// The spec caps a pointer file below 1024 bytes; anything larger is data.
pub const MAX_POINTER_BYTES: usize = 1024;

const IDENTIFIER: &str = "version https://git-lfs.github.com/spec/v1";
const OID_PREFIX: &str = "oid sha256:";
const SIZE_PREFIX: &str = "size ";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PointerError {
    #[error("content lacks the LFS pointer prefix")]
    MissingPrefix,
    #[error("pointer has an invalid structure")]
    InvalidStructure,
    #[error("pointer oid is malformed: {0}")]
    Oid(#[from] OidError),
    #[error("pointer size is malformed: {0:?}")]
    Size(String),
}

/// A parsed pointer: the identity and length of the object it stands for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    pub oid: Oid,
    pub size: u64,
}

impl Pointer {
    pub fn new(oid: Oid, size: u64) -> Self {
        Self { oid, size }
    }

    /// The canonical on-disk bytes. Keep byte-identical to Gitea's
    /// `Pointer.StringContent` — see the module comment.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    /// Cheap pre-filter before attempting a parse: real pointers are tiny, so
    /// a blob over the cap can be rejected without reading it.
    pub fn could_be_pointer(len: u64) -> bool {
        len > 0 && len <= MAX_POINTER_BYTES as u64
    }
}

/// Whether content of this length should become an LFS object at all.
///
/// Verified against git-lfs 3.x: `git lfs pointer --file` on a zero-length
/// file emits *nothing* on stdout, and the clean filter passes empty files
/// through unchanged. An empty file is therefore committed as an ordinary
/// empty git blob. Minting a pointer for it would produce an object no other
/// LFS client would ever create, and would make our checkout disagree with
/// `git checkout` on the same commit.
pub fn is_lfs_eligible(size: u64) -> bool {
    size > 0
}

impl fmt::Display for Pointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{IDENTIFIER}\n{OID_PREFIX}{}\n{SIZE_PREFIX}{}\n",
            self.oid, self.size
        )
    }
}

impl FromStr for Pointer {
    type Err = PointerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.starts_with(IDENTIFIER) {
            return Err(PointerError::MissingPrefix);
        }

        let mut lines = s.split('\n').skip(1);
        // The spec orders keys alphabetically after `version`, so oid precedes size.
        let oid_line = lines.next().ok_or(PointerError::InvalidStructure)?;
        let size_line = lines.next().ok_or(PointerError::InvalidStructure)?;

        let oid = oid_line
            .strip_prefix(OID_PREFIX)
            .ok_or(PointerError::InvalidStructure)?;
        let size = size_line
            .strip_prefix(SIZE_PREFIX)
            .ok_or(PointerError::InvalidStructure)?;

        Ok(Self {
            oid: Oid::new(oid)?,
            size: size
                .parse()
                .map_err(|_| PointerError::Size(size.to_owned()))?,
        })
    }
}

impl TryFrom<&[u8]> for Pointer {
    type Error = PointerError;

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        // Non-UTF8 means binary data, not a pointer.
        let s = std::str::from_utf8(buf).map_err(|_| PointerError::MissingPrefix)?;
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    fn sample() -> Pointer {
        Pointer::new(Oid::new(OID).unwrap(), 12345)
    }

    /// Locks the exact bytes Gitea's GC will hash. If this test is ever
    /// "fixed" by changing the expectation, tracked objects start disappearing.
    #[test]
    fn canonical_bytes_match_the_spec() {
        assert_eq!(
            sample().to_string(),
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{OID}\nsize 12345\n")
        );
    }

    #[test]
    fn round_trips() {
        let p = sample();
        assert_eq!(p.to_string().parse::<Pointer>().unwrap(), p);
    }

    #[test]
    fn rejects_non_pointer_content() {
        assert_eq!(
            "hello world".parse::<Pointer>(),
            Err(PointerError::MissingPrefix)
        );
    }

    #[test]
    fn rejects_binary_content() {
        assert_eq!(
            Pointer::try_from(&[0xff, 0xfe, 0x00][..]),
            Err(PointerError::MissingPrefix)
        );
    }

    #[test]
    fn rejects_truncated_pointer() {
        let truncated = format!("{IDENTIFIER}\n{OID_PREFIX}{OID}\n");
        // split('\n') yields a trailing "" here, which is not a size line.
        assert_eq!(
            truncated.parse::<Pointer>(),
            Err(PointerError::InvalidStructure)
        );
    }

    #[test]
    fn rejects_bad_size() {
        let bad = format!("{IDENTIFIER}\n{OID_PREFIX}{OID}\nsize twelve\n");
        assert_eq!(
            bad.parse::<Pointer>(),
            Err(PointerError::Size("twelve".into()))
        );
    }

    /// A size-0 pointer must still parse — Gitea's `IsValid` permits it and we
    /// may encounter one written by another tool. We just never mint one.
    #[test]
    fn accepts_empty_object_when_parsing() {
        let p = Pointer::new(Oid::new(OID).unwrap(), 0);
        assert_eq!(p.to_string().parse::<Pointer>().unwrap().size, 0);
    }

    #[test]
    fn empty_files_are_not_lfs_eligible() {
        assert!(!is_lfs_eligible(0));
        assert!(is_lfs_eligible(1));
    }

    #[test]
    fn size_prefilter_excludes_large_and_empty_blobs() {
        assert!(Pointer::could_be_pointer(130));
        assert!(!Pointer::could_be_pointer(0));
        assert!(!Pointer::could_be_pointer(MAX_POINTER_BYTES as u64 + 1));
    }
}
