//! The `ds.yaml` pipeline format.
//!
//! This is a contract, not an internal structure: Gitea's `modules/ds` parses
//! the same file to draw the DAG, and DVC's `dvc.yaml` is the same shape. The
//! polymorphic spellings below (`cmd` as scalar or list, an out as a bare path
//! or a single-key mapping) exist because DVC accepts them, so files in the
//! wild use them.

use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
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
    #[error("stage {0:?} is not defined in the pipeline")]
    UnknownStage(String),
}

type Result<T> = std::result::Result<T, PipelineError>;

/// Candidate file names, in the order they are looked for.
pub const FILE_NAMES: [&str; 2] = ["ds.yaml", "dvc.yaml"];

/// A parsed pipeline.
///
/// `stages` is an `IndexMap` rather than a `HashMap` because declaration order
/// is the tie-breaker for stage ordering and for the rendered DAG. A map that
/// reordered on every run would produce a different `ds.lock` from identical
/// inputs.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Pipeline {
    #[serde(default)]
    pub stages: IndexMap<String, Stage>,
}

/// One node of the pipeline.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Stage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: StringList,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wdir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: StringList,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outs: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plots: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
}

impl Pipeline {
    /// Reads the first pipeline file that exists under `root`.
    pub fn find(root: &Path) -> Result<Option<(String, Self)>> {
        for name in FILE_NAMES {
            let path = root.join(name);
            if path.is_file() {
                return Ok(Some((name.to_owned(), Self::read(&path)?)));
            }
        }
        Ok(None)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| PipelineError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        yaml_serde::from_str(text).map_err(|source| PipelineError::Parse {
            path: origin.to_owned(),
            source,
        })
    }

    pub fn stage(&self, name: &str) -> Result<&Stage> {
        self.stages
            .get(name)
            .ok_or_else(|| PipelineError::UnknownStage(name.to_owned()))
    }
}

impl Stage {
    /// Every path the stage writes, across outs, metrics and plots.
    pub fn out_paths(&self) -> Vec<&str> {
        self.outs
            .iter()
            .chain(&self.metrics)
            .chain(&self.plots)
            .map(|a| a.path.as_str())
            .collect()
    }

    /// The command as a shell would see it, one line per entry.
    pub fn command_text(&self) -> String {
        self.cmd.join("\n")
    }
}

/// A stage output. DVC writes these either as a bare path or as a single-key
/// mapping carrying options, and both spellings appear in real pipelines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub path: String,
    /// `cache: false` keeps a small artifact — a metrics file, usually — in
    /// git rather than LFS. `ds` only reads this to decide what it may expect
    /// to find as a pointer; moving the bytes is git's job either way.
    pub cache: bool,
    pub persist: bool,
}

impl Artifact {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            cache: true,
            persist: false,
        }
    }
}

/// Parameters a stage depends on, optionally scoped to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamRef {
    pub file: String,
    pub keys: Vec<String>,
}

/// The default parameter file, used when a stage names bare keys.
pub const DEFAULT_PARAMS_FILE: &str = "params.yaml";

/// A field that accepts a scalar or a sequence. `cmd` and `deps` both do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StringList(pub Vec<String>);

impl std::ops::Deref for StringList {
    type Target = Vec<String>;
    fn deref(&self) -> &Vec<String> {
        &self.0
    }
}

impl StringList {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

mod wire {
    //! Serde shims for the polymorphic spellings. Kept apart so the public
    //! types above stay readable.

    use super::*;

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum ScalarOrList {
        Scalar(String),
        List(Vec<String>),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum ArtifactWire {
        Path(String),
        Mapping(IndexMap<String, Option<ArtifactOptions>>),
    }

    #[derive(Deserialize)]
    pub struct ArtifactOptions {
        #[serde(default)]
        pub cache: Option<bool>,
        #[serde(default)]
        pub persist: bool,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum ParamWire {
        Key(String),
        Scoped(IndexMap<String, Option<Vec<String>>>),
    }
}

impl<'de> Deserialize<'de> for StringList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match wire::ScalarOrList::deserialize(d)? {
            wire::ScalarOrList::Scalar(s) => Self(vec![s]),
            wire::ScalarOrList::List(v) => Self(v),
        })
    }
}

impl Serialize for StringList {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        // Round-trips as a list even when it was read as a scalar; both parse.
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Artifact {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match wire::ArtifactWire::deserialize(d)? {
            wire::ArtifactWire::Path(path) => Self::new(path),
            wire::ArtifactWire::Mapping(map) => {
                // A single-key mapping by construction; anything further is a
                // malformed entry, and taking the first key is what DVC does.
                match map.into_iter().next() {
                    Some((path, options)) => Self {
                        path,
                        cache: options.as_ref().and_then(|o| o.cache).unwrap_or(true),
                        persist: options.as_ref().is_some_and(|o| o.persist),
                    },
                    None => Self::new(String::new()),
                }
            }
        })
    }
}

impl Serialize for Artifact {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.path.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ParamRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match wire::ParamWire::deserialize(d)? {
            wire::ParamWire::Key(key) => Self {
                file: DEFAULT_PARAMS_FILE.to_owned(),
                keys: vec![key],
            },
            wire::ParamWire::Scoped(map) => match map.into_iter().next() {
                Some((file, keys)) => Self {
                    file,
                    keys: keys.unwrap_or_default(),
                },
                None => Self {
                    file: DEFAULT_PARAMS_FILE.to_owned(),
                    keys: Vec::new(),
                },
            },
        })
    }
}

impl Serialize for ParamRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = IndexMap::new();
        map.insert(&self.file, &self.keys);
        map.serialize(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
stages:
  prepare:
    desc: Split the raw table
    cmd: python src/prepare.py
    deps:
      - src/prepare.py
      - data/raw/iris.csv
    params:
      - params.yaml:
          - prepare.seed
    outs:
      - data/prepared/train.csv
  evaluate:
    cmd:
      - python src/evaluate.py
      - echo done
    deps: data/prepared/train.csv
    metrics:
      - metrics.json:
          cache: false
    plots:
      - plots/confusion.json:
          cache: false
"#;

    fn sample() -> Pipeline {
        Pipeline::parse(SAMPLE, "ds.yaml").unwrap()
    }

    #[test]
    fn declaration_order_is_preserved() {
        let p = sample();
        let names: Vec<&str> = p.stages.keys().map(String::as_str).collect();
        assert_eq!(names, ["prepare", "evaluate"]);
    }

    #[test]
    fn cmd_accepts_a_scalar_or_a_list() {
        let p = sample();
        assert_eq!(
            p.stage("prepare").unwrap().command_text(),
            "python src/prepare.py"
        );
        assert_eq!(
            p.stage("evaluate").unwrap().command_text(),
            "python src/evaluate.py\necho done"
        );
    }

    #[test]
    fn deps_accept_a_scalar_or_a_list() {
        let p = sample();
        assert_eq!(p.stage("prepare").unwrap().deps.len(), 2);
        let evaluate = p.stage("evaluate").unwrap();
        assert_eq!(*evaluate.deps, ["data/prepared/train.csv".to_owned()]);
    }

    #[test]
    fn artifacts_accept_a_bare_path_or_options() {
        let p = sample();
        let prepared = &p.stage("prepare").unwrap().outs[0];
        assert_eq!(prepared.path, "data/prepared/train.csv");
        assert!(prepared.cache, "a bare path defaults to cached");

        let metric = &p.stage("evaluate").unwrap().metrics[0];
        assert_eq!(metric.path, "metrics.json");
        assert!(!metric.cache, "cache: false must survive the parse");
    }

    #[test]
    fn out_paths_span_outs_metrics_and_plots() {
        let p = sample();
        assert_eq!(
            p.stage("evaluate").unwrap().out_paths(),
            ["metrics.json", "plots/confusion.json"]
        );
    }

    #[test]
    fn params_accept_bare_keys_and_scoped_files() {
        let p = Pipeline::parse(
            "stages:\n  a:\n    cmd: x\n    params:\n      - seed\n      - other.yaml:\n          - k\n",
            "ds.yaml",
        )
        .unwrap();
        let params = &p.stage("a").unwrap().params;
        assert_eq!(params[0].file, DEFAULT_PARAMS_FILE);
        assert_eq!(params[0].keys, ["seed"]);
        assert_eq!(params[1].file, "other.yaml");
        assert_eq!(params[1].keys, ["k"]);
    }

    #[test]
    fn an_unknown_stage_is_named_in_the_error() {
        let err = sample().stage("nope").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn an_empty_document_is_an_empty_pipeline() {
        assert!(
            Pipeline::parse("stages: {}", "ds.yaml")
                .unwrap()
                .stages
                .is_empty()
        );
    }
}
