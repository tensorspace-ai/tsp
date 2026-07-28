//! Streaming sha256, producing the object identity used everywhere else.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::oid::Oid;
use crate::pointer::Pointer;

/// Read size for the hashing loop. Large enough that syscall overhead is
/// irrelevant on multi-GB files, small enough to stay in L2.
const CHUNK: usize = 512 * 1024;

/// Hashes a reader, returning the identity and byte count in one pass.
pub fn hash_reader(mut r: impl Read) -> io::Result<Pointer> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut size: u64 = 0;

    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }

    Ok(Pointer::new(
        Oid::from_bytes(&hasher.finalize().into()),
        size,
    ))
}

/// Hashes a file on disk.
pub fn hash_file(path: impl AsRef<Path>) -> io::Result<Pointer> {
    hash_reader(File::open(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The well-known sha256 of the empty input. git-lfs uses this oid for
    /// zero-length files, so it is worth pinning.
    const EMPTY_OID: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_empty_input() {
        let p = hash_reader(&b""[..]).unwrap();
        assert_eq!(p.oid.as_str(), EMPTY_OID);
        assert_eq!(p.size, 0);
    }

    #[test]
    fn hashes_known_vector() {
        let p = hash_reader(&b"abc"[..]).unwrap();
        assert_eq!(
            p.oid.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(p.size, 3);
    }

    /// Guards the chunking loop: a payload spanning several reads must hash
    /// identically to the same bytes hashed in one go.
    #[test]
    fn chunk_boundaries_do_not_affect_the_digest() {
        let data = vec![7u8; CHUNK * 2 + 13];
        let streamed = hash_reader(&data[..]).unwrap();
        let oneshot = Oid::from_bytes(&Sha256::digest(&data).into());
        assert_eq!(streamed.oid, oneshot);
        assert_eq!(streamed.size, data.len() as u64);
    }
}
