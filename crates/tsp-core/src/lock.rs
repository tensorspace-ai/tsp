//! The `tsp.lock` format: what each stage was run against.
//!
//! The lock answers one question — has anything a stage depends on moved since
//! it last ran — and carries only what is needed to answer it. Everything else
//! about a run is already in git: what the outputs contain, how big they are,
//! which commit they landed in. The one thing git cannot state is that a stage
//! *was executed*, and against which content, because a run happens before any
//! commit exists to record it.
//!
//! So an entry is a command, the parameter values it read, and the git object
//! id of each dependency:
//!
//! ```yaml
//! schema: 3
//! stages:
//!   train:
//!     cmd: python src/train.py
//!     params:
//!       params.yaml:
//!         train.max_depth: 4
//!     deps:
//!       src/train.py: e753b03a96da287cb864f732b70a4d17329e6277
//!       data/prepared/train.csv: ab70643141a7717ac63c98bc9d26395660196ef2
//! ```
//!
//! An object id is the identity of the content, so nothing here duplicates a
//! digest git already stores — for an LFS-tracked path the id is the pointer's,
//! and the pointer states the sha256 itself.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("cannot read {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse {path}")]
    Parse {
        path: String,
        #[source]
        source: yaml_serde::Error,
    },
    #[error("cannot serialise the lock: {0}")]
    Write(#[from] yaml_serde::Error),
}

type Result<T> = std::result::Result<T, LockError>;

/// The one lock file name. A `dvc.yaml` pipeline locks here too: the format is
/// not DVC's, so writing `dvc.lock` would only mislead a reader.
pub const FILE_NAME: &str = "tsp.lock";

/// Bumped whenever the shape changes, so a future reader can tell rather than
/// guess. Version 3 is the first that is not DVC-compatible.
pub const SCHEMA: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lock {
    pub schema: u32,
    #[serde(default)]
    pub stages: IndexMap<String, LockStage>,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            stages: IndexMap::new(),
        }
    }
}

/// One stage's recorded run.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct LockStage {
    pub cmd: String,
    /// The parameter values the run read, by file then dotted key.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub params: IndexMap<String, IndexMap<String, serde_json::Value>>,
    /// Each dependency's git object id at the time of the run.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub deps: IndexMap<String, String>,
}

impl Lock {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Reads the repository's lock, or a fresh one when it has never been run.
    pub fn read_or_default(root: &std::path::Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        if path.is_file() {
            Self::read(&path)
        } else {
            Ok(Self::default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Lock {
        let mut lock = Lock::default();
        lock.stages.insert(
            "train".to_owned(),
            LockStage {
                cmd: "python src/train.py".to_owned(),
                params: IndexMap::from([(
                    "params.yaml".to_owned(),
                    IndexMap::from([("train.max_depth".to_owned(), json!(4))]),
                )]),
                deps: IndexMap::from([("src/train.py".to_owned(), "b".repeat(40))]),
            },
        );
        lock
    }

    #[test]
    fn round_trips_through_yaml() {
        let back = Lock::parse(&sample().to_yaml().unwrap(), "tsp.lock").unwrap();

        assert_eq!(back.schema, SCHEMA);
        let stage = &back.stages["train"];
        assert_eq!(stage.cmd, "python src/train.py");
        assert_eq!(stage.deps["src/train.py"], "b".repeat(40));
        assert_eq!(stage.params["params.yaml"]["train.max_depth"], json!(4));
    }

    /// A dependency is one line: a path and the id of what it held.
    #[test]
    fn a_dependency_is_a_path_and_an_id() {
        let text = sample().to_yaml().unwrap();
        assert!(
            text.contains(&format!("src/train.py: {}", "b".repeat(40))),
            "{text}"
        );
        for absent in [
            "sha256", "md5", "size", "nfiles", "hash:", "outs", "metrics",
        ] {
            assert!(!text.contains(absent), "{absent} should be gone:\n{text}");
        }
    }

    /// Empty groups must not appear; a lock full of `params: {}` is noise in
    /// every diff.
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
        assert!(!text.contains("deps"), "{text}");
        assert!(!text.contains("params"), "{text}");
    }

    #[test]
    fn stage_order_survives_a_round_trip() {
        let mut lock = Lock::default();
        for name in ["prepare", "train", "evaluate"] {
            lock.stages.insert(name.to_owned(), LockStage::default());
        }

        let back = Lock::parse(&lock.to_yaml().unwrap(), "tsp.lock").unwrap();
        let names: Vec<&str> = back.stages.keys().map(String::as_str).collect();
        assert_eq!(names, ["prepare", "train", "evaluate"]);
    }

    /// Dependency order follows the pipeline, so the file diffs predictably.
    #[test]
    fn dependency_order_is_preserved() {
        let mut lock = Lock::default();
        lock.stages.insert(
            "a".to_owned(),
            LockStage {
                cmd: "x".to_owned(),
                deps: IndexMap::from([
                    ("z.py".to_owned(), "1".repeat(40)),
                    ("a.csv".to_owned(), "2".repeat(40)),
                ]),
                ..Default::default()
            },
        );

        let back = Lock::parse(&lock.to_yaml().unwrap(), "tsp.lock").unwrap();
        let paths: Vec<&str> = back.stages["a"].deps.keys().map(String::as_str).collect();
        assert_eq!(paths, ["z.py", "a.csv"]);
    }
}
