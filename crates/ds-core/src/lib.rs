//! Core domain types for `ds`: object identity, the git-lfs pointer format,
//! hashing, and the local content-addressed cache.
//!
//! `ds` deliberately does *not* install a `filter=lfs` gitattribute. It writes
//! pointer blobs into git itself and owns transfer, so that a plain `git clone`
//! yields pointers rather than triggering a smudge of every tracked byte.

pub mod cache;
pub mod git;
pub mod hash;
pub mod oid;
pub mod paths;
pub mod pointer;

pub use cache::Cache;
pub use oid::Oid;
pub use pointer::Pointer;
