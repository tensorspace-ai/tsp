//! Git LFS client: endpoint derivation and the batch transfer protocol.
//!
//! `ds` uses the standard LFS batch API as its remote, so the same code works
//! against Gitea, GitHub and GitLab. Auth comes from `git credential`, meaning
//! there is no `ds remote add` step to get wrong.

pub mod client;
pub mod endpoint;
pub mod wire;

pub use client::{Client, ClientError};
