//! The `ds.lock` format: what each stage resolved to the last time it ran.
//!
//! The lock is what makes staleness computable without re-running anything. Its
//! reader is not only `ds status` but Gitea's Data tab, which colours the DAG
//! from it on every page view — so every entry carries `git_sha`, the git object
//! id of the path at the recorded commit. Comparing object ids is a tree lookup;
//! comparing content hashes would mean hashing the dataset on a page load.
//!
//! `sha256` is recorded alongside for DVC compatibility and for display. For an
//! LFS-tracked path it is read straight out of the pointer blob rather than
//! computed, which is why locking a multi-gigabyte output costs nothing.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: yaml_serde::Error,
    },
    #[error("cannot serialise the lock: {0}")]
    Write(#[from] yaml_serde::Error),
}

type Result<T> = std::result::Result<T, LockError>;

/// Candidate file names, paired positionally with `pipeline::FILE_NAMES`.
pub const FILE_NAMES: [&str; 2] = ["ds.lock", "dvc.lock"];

/// The schema version written into new locks. DVC 2.x uses the same value, and
/// Gitea reads the field without switching on it.
pub const SCHEMA: &str = "2.0";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lock {
    pub schema: String,
    #[serde(default)]
    pub stages: IndexMap<String, LockStage>,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            stages: IndexMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LockStage {
    pub cmd: String,
    /// Resolved parameter values, keyed by file then by dotted key. Recorded so
    /// a run can be reproduced from the lock alone.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub params: IndexMap<String, IndexMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<LockEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outs: Vec<LockEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<LockEntry>,
}

/// One resolved path.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LockEntry {
    pub path: String,
    /// Names which digest field is authoritative. Always `sha256` when written
    /// here; DVC-written locks say `md5`, which is why the field exists at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Set when the entry stands for a directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nfiles: Option<usize>,
    /// The git object id of this path, which is what staleness is decided on.
    /// Absent in DVC-written locks; those stages report unknown rather than
    /// being hashed on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
}

impl Lock {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Reads the lock beside `pipeline_file`, or a fresh one if none exists.
    pub fn read_or_default(root: &std::path::Path, pipeline_file: &str) -> Result<Self> {
        let name = Self::name_for(pipeline_file);
        let path = root.join(name);
        if path.is_file() {
            Self::read(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// `ds.yaml` locks into `ds.lock`, `dvc.yaml` into `dvc.lock`.
    pub fn name_for(pipeline_file: &str) -> &'static str {
        match crate::pipeline::FILE_NAMES
            .iter()
            .position(|n| *n == pipeline_file)
        {
            Some(i) => FILE_NAMES[i],
            None => FILE_NAMES[0],
        }
    }

    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        yaml_serde::from_str(text).map_err(|source| LockError::Parse {
            path: origin.to_owned(),
            source,
        })
    }

    pub fn to_yaml(&self) -> Result<String> {
        Ok(yaml_serde::to_string(self)?)
    }

    pub fn write(&self, path: &std::path::Path) -> Result<()> {
        let text = self.to_yaml()?;
        std::fs::write(path, text).map_err(|source| LockError::Read {
            path: path.display().to_string(),
            source,
        })
    }
}

impl LockStage {
    /// Finds an entry for `path` across every group the stage records.
    pub fn entry(&self, path: &str) -> Option<&LockEntry> {
        self.deps
            .iter()
            .chain(&self.outs)
            .chain(&self.metrics)
            .find(|e| e.path == path)
    }
}

impl LockEntry {
    /// The authoritative digest, whichever algorithm the file declared.
    pub fn digest(&self) -> Option<&str> {
        match self.hash.as_deref() {
            Some("sha256") => self.sha256.as_deref(),
            Some("md5") => self.md5.as_deref(),
            _ => self.sha256.as_deref().or(self.md5.as_deref()),
        }
    }

    pub fn is_dir(&self) -> bool {
        self.nfiles.is_some() || self.digest().is_some_and(|d| d.ends_with(".dir"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_yaml() {
        let mut lock = Lock::default();
        lock.stages.insert(
            "train".to_owned(),
            LockStage {
                cmd: "python src/train.py".to_owned(),
                deps: vec![LockEntry {
                    path: "src/train.py".to_owned(),
                    hash: Some("sha256".to_owned()),
                    sha256: Some("a".repeat(64)),
                    size: Some(919),
                    git_sha: Some("b".repeat(40)),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        let text = lock.to_yaml().unwrap();
        let back = Lock::parse(&text, "ds.lock").unwrap();

        assert_eq!(back.schema, SCHEMA);
        let stage = &back.stages["train"];
        assert_eq!(stage.cmd, "python src/train.py");
        assert_eq!(
            stage.deps[0].git_sha.as_deref(),
            Some("b".repeat(40).as_str())
        );
        assert_eq!(stage.deps[0].digest(), Some("a".repeat(64).as_str()));
    }

    /// Empty groups must not appear in the file; a lock full of `outs: []` is
    /// noise in every diff.
    #[test]
    fn empty_groups_are_omitted() {
        let mut lock = Lock::default();
        lock.stages.insert(
            "a".to_owned(),
            LockStage {
                cmd: "x".to_owned(),
                ..Default::default()
            },
        );
        let text = lock.to_yaml().unwrap();
        assert!(!text.contains("outs"), "{text}");
        assert!(!text.contains("params"), "{text}");
    }

    #[test]
    fn stage_order_survives_a_round_trip() {
        let mut lock = Lock::default();
        for name in ["prepare", "train", "evaluate"] {
            lock.stages.insert(name.to_owned(), LockStage::default());
        }
        let back = Lock::parse(&lock.to_yaml().unwrap(), "ds.lock").unwrap();
        let names: Vec<&str> = back.stages.keys().map(String::as_str).collect();
        assert_eq!(names, ["prepare", "train", "evaluate"]);
    }

    /// DVC writes md5 and no git id; those entries must still read back.
    #[test]
    fn a_dvc_written_lock_parses() {
        let text = "schema: '2.0'\nstages:\n  train:\n    cmd: python train.py\n    outs:\n    - path: model.pkl\n      md5: d41d8cd98f00b204e9800998ecf8427e\n      size: 12\n";
        let lock = Lock::parse(text, "dvc.lock").unwrap();
        let entry = &lock.stages["train"].outs[0];
        assert_eq!(entry.digest(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert!(entry.git_sha.is_none());
    }

    #[test]
    fn a_directory_entry_is_recognised() {
        let dir = LockEntry {
            nfiles: Some(120),
            ..Default::default()
        };
        assert!(dir.is_dir());

        let dvc_dir = LockEntry {
            sha256: Some("abc.dir".to_owned()),
            ..Default::default()
        };
        assert!(dvc_dir.is_dir());
    }

    #[test]
    fn lock_name_follows_the_pipeline_name() {
        assert_eq!(Lock::name_for("ds.yaml"), "ds.lock");
        assert_eq!(Lock::name_for("dvc.yaml"), "dvc.lock");
    }
}
