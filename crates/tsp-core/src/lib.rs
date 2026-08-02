//! Core domain types for `tsp`: the pipeline and lock formats, parameters, the
//! stage graph, and the git plumbing they are read through.
//!
//! `tsp` does not manage data. Datasets live in Git LFS, tracked by an ordinary
//! `filter=lfs` gitattribute, so `git add`, `git push` and `git checkout` move
//! bytes with no help from this tool. What is left — and what this crate is
//! about — is the layer above: which stages produced which artifacts, whether
//! that record still applies, and what an experiment changed.
//!
//! The `tsp.yaml` and `tsp.lock` formats are the contract with Gitea's Data tab,
//! which parses both to render the DAG. They stay compatible with DVC's
//! `dvc.yaml`/`dvc.lock` so existing repositories work unchanged.

pub mod expand;
pub mod figure;
pub mod git;
pub mod graph;
pub mod hash;
pub mod interp;
pub mod lock;
pub mod metrics;
pub mod oid;
pub mod params;
pub mod paths;
pub mod pipeline;
pub mod plotdata;
pub mod plots;
pub mod pointer;
pub mod svg;

pub use lock::{Lock, LockStage};
pub use oid::Oid;
pub use pipeline::{Pipeline, Stage};
pub use pointer::Pointer;
