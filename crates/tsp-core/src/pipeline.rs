//! The `tsp.yaml` pipeline format.
//!
//! This is a contract, not an internal structure: Gitea's `modules/tsp` parses
//! the same file to draw the DAG, and DVC's `dvc.yaml` is the same shape. The
//! polymorphic spellings below (`cmd` as scalar or list, an out as a bare path
//! or a single-key mapping) exist because DVC accepts them, so files in the
//! wild use them.
//!
//! An optional `schema:` key states which shape the file is written in. It is
//! absent from every pipeline that exists today, and from `dvc.yaml` entirely,
//! so absence means 1 — but declaring it now is what lets a later change be
//! rejected outright instead of silently parsing to a different DAG.

use std::path::Path;

use indexmap::IndexMap;

use crate::plots::{self, Plot};
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
    #[error(
        "{path} is schema {found} and this tsp understands {SCHEMA}; upgrade tsp to read this pipeline"
    )]
    UnsupportedSchema { path: String, found: u32 },
    #[error("{path}: unknown key {key:?} in {location}{hint}")]
    UnknownKey {
        path: String,
        location: String,
        key: String,
        hint: String,
    },
    #[error("{path}: stage {name:?} has no cmd, so there is nothing to bring up to date")]
    NoCommand { path: String, name: String },
}

type Result<T> = std::result::Result<T, PipelineError>;

/// Candidate file names, in the order they are looked for.
///
/// `dvc.yaml` is the same shape written by DVC, and reading it is how an
/// existing DVC repository renders without being migrated first.
pub const FILE_NAMES: [&str; 2] = ["tsp.yaml", "dvc.yaml"];

/// The pipeline shape this version understands.
///
/// The field is optional and absence means 1, because every pipeline written
/// before it existed is schema 1 and `dvc.yaml` has no such key at all. It is
/// declared now, while there is only one shape, so that a later change is
/// something a reader can *detect* rather than infer from a parse that half
/// worked.
pub const SCHEMA: u32 = 1;

fn default_schema() -> u32 {
    SCHEMA
}

/// The keys a pipeline file may carry at the top level.
pub const PIPELINE_KEYS: [&str; 3] = ["schema", "stages", "plots"];

/// The keys a stage may carry.
pub const STAGE_KEYS: [&str; 8] = [
    "cmd", "wdir", "deps", "outs", "metrics", "plots", "params", "desc",
];

/// Why a key DVC defines is refused rather than ignored.
///
/// These parse as the shape they are and mean nothing to this tool, which is
/// the dangerous combination: a `foreach` stage keeps its command under `do:`,
/// so dropping the key leaves a stage with an empty `cmd` — one that runs
/// nothing and is judged current the moment it has "run".
fn dvc_hint(key: &str) -> Option<&'static str> {
    match key {
        "foreach" | "matrix" | "do" => Some(
            "tsp does not expand templated stages; write them out, or keep running this pipeline with dvc",
        ),
        "vars" => Some("tsp does not substitute variables"),
        "frozen" | "always_changed" => {
            Some("tsp decides staleness from the lock alone, so this would be ignored")
        }
        "artifacts" => Some("tsp has no artifact registry"),
        _ => None,
    }
}

/// Rejects keys this version does not define, before the typed parse.
///
/// A generic pass first is what lets the message name the key *and* the stage
/// it sits in. It matters most for the keys that are neither typos nor
/// supported: silently dropping `foreach` produces a DAG that runs nothing and
/// reports success, which is the one answer this tool must never give.
fn reject_unknown_keys(text: &str, path: &str) -> Result<()> {
    // A syntax error is the typed parse's to report, with its line and column.
    let Ok(root) = yaml_serde::from_str::<serde_json::Value>(text) else {
        return Ok(());
    };
    let Some(top) = root.as_object() else {
        return Ok(());
    };
    let unknown = |location: String, key: &str, known: &[&str]| PipelineError::UnknownKey {
        path: path.to_owned(),
        location,
        key: key.to_owned(),
        hint: match dvc_hint(key) {
            Some(hint) => format!("; {hint}"),
            None => format!("; known keys are {}", known.join(", ")),
        },
    };
    for key in top.keys() {
        if !PIPELINE_KEYS.contains(&key.as_str()) {
            return Err(unknown("the pipeline".to_owned(), key, &PIPELINE_KEYS));
        }
    }
    let Some(stages) = top.get("stages").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for (name, stage) in stages {
        let Some(stage) = stage.as_object() else {
            continue;
        };
        for key in stage.keys() {
            if !STAGE_KEYS.contains(&key.as_str()) {
                return Err(unknown(format!("stage {name:?}"), key, &STAGE_KEYS));
            }
        }
    }
    Ok(())
}

/// A parsed pipeline.
///
/// `stages` is an `IndexMap` rather than a `HashMap` because declaration order
/// is the tie-breaker for stage ordering and for the rendered DAG. A map that
/// reordered on every run would produce a different `tsp.lock` from identical
/// inputs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub stages: IndexMap<String, Stage>,
    /// Plots declared for the pipeline as a whole. Unlike a stage's own plots
    /// these are not artifacts: an entry may carry a display name and pull its
    /// data from several files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plots: TopPlots,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            stages: IndexMap::new(),
            plots: TopPlots::default(),
        }
    }
}

/// One node of the pipeline.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    pub plots: Vec<PlotArtifact>,
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
        reject_unknown_keys(text, origin)?;
        let pipeline: Self = yaml_serde::from_str(text).map_err(|source| PipelineError::Parse {
            path: origin.to_owned(),
            source,
        })?;
        // Unlike the lock, which only records what the last run saw and can be
        // thrown away for the cost of a rerun, the pipeline *is* the user's
        // definition. A shape this version cannot read must stop the command
        // rather than yield a DAG missing whatever the new schema added.
        if pipeline.schema > SCHEMA {
            return Err(PipelineError::UnsupportedSchema {
                path: origin.to_owned(),
                found: pipeline.schema,
            });
        }
        // A stage with no command runs nothing, and the staleness check skips
        // the command comparison when there is none to make — so it would go
        // straight to current and stay there. Refusing it here is what keeps
        // "current" meaning "the recorded result still holds".
        for (name, stage) in &pipeline.stages {
            if stage.cmd.is_empty() {
                return Err(PipelineError::NoCommand {
                    path: origin.to_owned(),
                    name: name.clone(),
                });
            }
        }
        Ok(pipeline)
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
            .map(|a| a.path.as_str())
            .chain(self.plots.iter().map(|p| p.artifact.path.as_str()))
            .collect()
    }

    /// The plots this stage declares, normalised for drawing.
    pub fn plot_defs(&self) -> Vec<&Plot> {
        self.plots.iter().map(|p| &p.plot).collect()
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
    /// git rather than LFS. `tsp` only reads this to decide what it may expect
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

impl Default for Artifact {
    /// `cache: true` is the default a bare path implies, so an empty artifact
    /// has to agree with `new` rather than with `bool::default`.
    fn default() -> Self {
        Self::new(String::new())
    }
}

/// A `plots:` entry inside a stage: an artifact that also says how to draw
/// itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlotArtifact {
    pub artifact: Artifact,
    pub plot: Plot,
}

impl<'de> Deserialize<'de> for PlotArtifact {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let (artifact, options) = match wire::PlotWire::deserialize(d)? {
            wire::PlotWire::Path(path) => (Artifact::new(path), plots::Options::default()),
            wire::PlotWire::Mapping(map) => match map.into_iter().next() {
                Some((path, options)) => {
                    let options = options.unwrap_or_default();
                    (
                        Artifact {
                            path: path.clone(),
                            cache: options.cache.unwrap_or(true),
                            persist: options.persist.unwrap_or(false),
                        },
                        options,
                    )
                }
                None => (Artifact::new(String::new()), plots::Options::default()),
            },
        };

        let plot = plots::from_artifact(&artifact.path, &options);
        Ok(Self { artifact, plot })
    }
}

impl Serialize for PlotArtifact {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.artifact.path.serialize(s)
    }
}

/// The top-level `plots:` section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopPlots(pub Vec<Plot>);

impl std::ops::Deref for TopPlots {
    type Target = Vec<Plot>;
    fn deref(&self) -> &Vec<Plot> {
        &self.0
    }
}

impl TopPlots {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for TopPlots {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let entries = Vec::<wire::PlotWire>::deserialize(d)?;
        Ok(Self(
            entries
                .into_iter()
                .filter_map(|entry| match entry {
                    wire::PlotWire::Path(path) => {
                        Some(plots::from_top_level(&path, &plots::Options::default()))
                    }
                    wire::PlotWire::Mapping(map) => {
                        map.into_iter().next().map(|(name, options)| {
                            plots::from_top_level(&name, &options.unwrap_or_default())
                        })
                    }
                })
                .collect(),
        ))
    }
}

impl Serialize for TopPlots {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let names: Vec<&str> = self.0.iter().map(|p| p.name.as_str()).collect();
        names.serialize(s)
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

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum PlotWire {
        Path(String),
        Mapping(IndexMap<String, Option<plots::Options>>),
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
        Pipeline::parse(SAMPLE, "tsp.yaml").unwrap()
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
            "tsp.yaml",
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

    /// The whole point of refusing these: dropping `foreach` leaves a stage
    /// whose command is empty, which runs nothing and then reports itself
    /// current. Erroring is the only honest answer.
    #[test]
    fn a_dvc_foreach_stage_is_refused_and_says_why() {
        let err = Pipeline::parse(
            "stages:\n  train:\n    foreach: [a, b]\n    do:\n      cmd: train ${item}\n",
            "dvc.yaml",
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("foreach"), "{text}");
        assert!(text.contains("train"), "names the stage: {text}");
        assert!(text.contains("dvc"), "points somewhere useful: {text}");
    }

    #[test]
    fn a_misspelled_stage_key_is_refused_and_lists_the_real_ones() {
        let err = Pipeline::parse("stages:\n  a:\n    cmd: x\n    outz: [m.bin]\n", "tsp.yaml")
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("outz"), "{text}");
        assert!(text.contains("outs"), "lists the known keys: {text}");
    }

    /// `stagez:` used to parse as a pipeline with no stages at all, so the tool
    /// reported "nothing to do" about a file that plainly defined work.
    #[test]
    fn a_misspelled_top_level_key_is_refused() {
        let err = Pipeline::parse("stagez:\n  a:\n    cmd: x\n", "tsp.yaml").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("stagez"), "{text}");
        assert!(text.contains("stages"), "lists the known keys: {text}");
    }

    #[test]
    fn a_stage_with_no_command_is_refused() {
        let err = Pipeline::parse("stages:\n  a:\n    outs: [m.bin]\n", "tsp.yaml").unwrap_err();
        let text = err.to_string();
        assert!(text.contains('a'), "{text}");
        assert!(text.contains("cmd"), "{text}");
    }

    #[test]
    fn a_syntax_error_is_still_reported_as_one() {
        let err = Pipeline::parse("stages: [unclosed\n", "tsp.yaml").unwrap_err();
        assert!(
            matches!(err, PipelineError::Parse { .. }),
            "expected a parse error, got {err}"
        );
    }

    /// A stage's plot entry is both an artifact and a drawing instruction, and
    /// the two must not interfere: `cache` belongs to the artifact, `template`
    /// to the plot.
    #[test]
    fn a_stage_plot_carries_both_storage_and_drawing_options() {
        let p = Pipeline::parse(
            "stages:\n  evaluate:\n    cmd: e\n    plots:\n      - plots/confusion.json:\n          template: confusion\n          x: predicted\n          y: actual\n          cache: false\n",
            "tsp.yaml",
        )
        .unwrap();

        let entry = &p.stage("evaluate").unwrap().plots[0];
        assert_eq!(entry.artifact.path, "plots/confusion.json");
        assert!(
            !entry.artifact.cache,
            "cache: false must reach the artifact"
        );
        assert_eq!(entry.plot.template, plots::Template::Confusion);
        assert_eq!(entry.plot.sources[0].x.as_deref(), Some("predicted"));
    }

    #[test]
    fn a_bare_plot_path_still_produces_a_plot() {
        let p = Pipeline::parse(
            "stages:\n  a:\n    cmd: x\n    plots: [plots/loss.csv]\n",
            "tsp.yaml",
        )
        .unwrap();

        let entry = &p.stage("a").unwrap().plots[0];
        assert!(entry.artifact.cache);
        assert_eq!(entry.plot.template, plots::Template::Linear);
        assert_eq!(entry.plot.files(), ["plots/loss.csv"]);
    }

    #[test]
    fn plots_are_still_stage_outputs() {
        let p = Pipeline::parse(
            "stages:\n  a:\n    cmd: x\n    outs: [m.bin]\n    metrics: [m.json]\n    plots: [p.csv]\n",
            "tsp.yaml",
        )
        .unwrap();
        assert_eq!(
            p.stage("a").unwrap().out_paths(),
            ["m.bin", "m.json", "p.csv"]
        );
    }

    #[test]
    fn a_top_level_plot_section_is_parsed() {
        let p = Pipeline::parse(
            "stages:\n  a:\n    cmd: x\nplots:\n  - Precision-Recall:\n      template: smooth\n      x: recall\n      y:\n        eval/prc.json: precision\n  - plots/roc.csv:\n      x: fpr\n      y: tpr\n",
            "tsp.yaml",
        )
        .unwrap();

        assert_eq!(p.plots.len(), 2);
        assert_eq!(p.plots[0].name, "Precision-Recall");
        assert_eq!(p.plots[0].template, plots::Template::Smooth);
        assert_eq!(p.plots[0].files(), ["eval/prc.json"]);
        assert_eq!(p.plots[1].files(), ["plots/roc.csv"]);
    }

    #[test]
    fn a_pipeline_without_plots_has_none() {
        let p = Pipeline::parse("stages:\n  a:\n    cmd: x\n", "tsp.yaml").unwrap();
        assert!(p.plots.is_empty());
    }

    #[test]
    fn an_empty_document_is_an_empty_pipeline() {
        assert!(
            Pipeline::parse("stages: {}", "tsp.yaml")
                .unwrap()
                .stages
                .is_empty()
        );
    }

    /// Every pipeline in existence predates the field, and `dvc.yaml` will
    /// never carry it, so absence has to mean the shape we already read.
    #[test]
    fn a_pipeline_without_a_schema_is_schema_one() {
        let p = Pipeline::parse("stages:\n  a:\n    cmd: x\n", "dvc.yaml").unwrap();
        assert_eq!(p.schema, 1);
    }

    #[test]
    fn the_current_schema_is_accepted_when_stated() {
        let p = Pipeline::parse("schema: 1\nstages:\n  a:\n    cmd: x\n", "tsp.yaml").unwrap();
        assert_eq!(p.schema, 1);
        assert_eq!(p.stages.len(), 1);
    }

    /// A pipeline is the user's definition, not a cache: reading a newer one
    /// partially would run a DAG that is missing whatever the new schema added,
    /// which is worse than refusing.
    #[test]
    fn a_newer_schema_is_refused_rather_than_read_partially() {
        let err =
            Pipeline::parse("schema: 2\nstages:\n  a:\n    cmd: x\n", "tsp.yaml").unwrap_err();

        assert!(
            matches!(err, PipelineError::UnsupportedSchema { found: 2, .. }),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("tsp.yaml") && message.contains("schema 2"),
            "{message}"
        );
    }
}
