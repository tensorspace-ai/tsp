//! Parameter files and the overrides an experiment applies to them.
//!
//! Values are held as JSON rather than a YAML-specific type: the lock records
//! them, metrics files are usually JSON, and keeping one scalar model across
//! both avoids a second set of number-formatting rules.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::git::{Git, GitError};
use crate::pipeline::Pipeline;

#[derive(Debug, thiserror::Error)]
pub enum ParamsError {
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
    #[error("{0:?} is not `key=value`")]
    MalformedOverride(String),
    #[error("cannot set {key:?}: {segment:?} is a scalar, not a mapping")]
    NotAMapping { key: String, segment: String },
}

type Result<T> = std::result::Result<T, ParamsError>;

/// A parameter value read for display.
///
/// Aliased to `metrics::Metric` deliberately: to a comparison a parameter and a
/// metric are the same thing — a file, a dotted key and a scalar — and
/// `tsp params --compare` must line its columns up exactly as
/// `tsp metrics --compare` does. Sharing the type is what stops the two
/// drifting apart.
pub type Param = crate::metrics::Metric;

/// Reads the parameter values the pipeline's stages declare, at the working
/// tree when `rev` is `None` or at that revision otherwise.
///
/// Only the keys stages declare, never every key in the file. Those are the
/// ones staleness is decided from, and a listing that showed more would
/// disagree with `tsp status` about which parameters matter.
///
/// A file that is absent or unparseable contributes nothing rather than
/// failing, matching `metrics::read` and deliberately unlike `Repo::param_cache`,
/// which errors. The difference is what each is for: staleness must not be
/// decided from a file that could not be read, but a comparison against a
/// three-month-old commit must not fail because that commit had a typo.
pub fn read(
    git: &Git,
    root: &Path,
    pipeline: &Pipeline,
    rev: Option<&str>,
) -> std::result::Result<Vec<Param>, GitError> {
    let mut out: Vec<Param> = Vec::new();
    let mut files: BTreeMap<String, Option<Params>> = BTreeMap::new();

    for stage in pipeline.stages.values() {
        for reference in &stage.params {
            // Several stages usually read the same file; read it once.
            if !files.contains_key(&reference.file) {
                let raw: Option<Vec<u8>> = match rev {
                    Some(rev) => git.read_blob_at(rev, &reference.file)?,
                    None => std::fs::read(root.join(&reference.file)).ok(),
                };
                let parsed = raw.and_then(|raw| {
                    Params::parse(&String::from_utf8_lossy(&raw), &reference.file).ok()
                });
                files.insert(reference.file.clone(), parsed);
            }
            let Some(Some(params)) = files.get(&reference.file) else {
                continue;
            };

            for key in &reference.keys {
                let Some(value) = params.get(key) else {
                    continue;
                };
                // Two stages may declare the same key; it is one column.
                if out
                    .iter()
                    .any(|p| p.file == reference.file && &p.key == key)
                {
                    continue;
                }
                out.push(Param {
                    file: reference.file.clone(),
                    key: key.clone(),
                    value: value.clone(),
                });
            }
        }
    }

    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.key.cmp(&b.key)));
    Ok(out)
}

/// One parameter file, parsed.
#[derive(Clone, Debug, Default)]
pub struct Params {
    root: Value,
}

impl Params {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| ParamsError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Reads a file that need not exist; a missing one yields no parameters.
    pub fn read_optional(path: &Path) -> Result<Self> {
        if path.is_file() {
            Self::read(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        let root: Value = yaml_serde::from_str(text).map_err(|source| ParamsError::Parse {
            path: origin.to_owned(),
            source,
        })?;
        Ok(Self { root })
    }

    /// Looks up a dotted key, e.g. `train.max_depth`.
    pub fn get(&self, dotted: &str) -> Option<&Value> {
        let mut node = &self.root;
        for segment in dotted.split('.') {
            node = node.get(segment)?;
        }
        Some(node)
    }

    /// Sets a dotted key, creating intermediate mappings as needed.
    pub fn set(&mut self, dotted: &str, value: Value) -> Result<()> {
        if !self.root.is_object() {
            self.root = Value::Object(serde_json::Map::new());
        }
        let mut node = &mut self.root;
        let segments: Vec<&str> = dotted.split('.').collect();

        for segment in &segments[..segments.len() - 1] {
            let map = node
                .as_object_mut()
                .ok_or_else(|| ParamsError::NotAMapping {
                    key: dotted.to_owned(),
                    segment: (*segment).to_owned(),
                })?;
            node = map
                .entry(*segment)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }

        let last = segments[segments.len() - 1];
        node.as_object_mut()
            .ok_or_else(|| ParamsError::NotAMapping {
                key: dotted.to_owned(),
                segment: last.to_owned(),
            })?
            .insert(last.to_owned(), value);
        Ok(())
    }

    pub fn to_yaml(&self) -> Result<String> {
        yaml_serde::to_string(&self.root).map_err(|source| ParamsError::Parse {
            path: "params".to_owned(),
            source,
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_yaml()?).map_err(|source| ParamsError::Read {
            path: path.display().to_string(),
            source,
        })
    }
}

/// A `key=value` override collected from the command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Override {
    /// The file the key lives in. Overrides may be scoped as
    /// `other.yaml:key=value`; unscoped ones land in `params.yaml`.
    pub file: String,
    pub key: String,
    pub value: Value,
}

impl std::str::FromStr for Override {
    type Err = ParamsError;

    fn from_str(text: &str) -> Result<Self> {
        let (lhs, raw) = text
            .split_once('=')
            .ok_or_else(|| ParamsError::MalformedOverride(text.to_owned()))?;

        // A colon before the key scopes it to a file. Split on the last one so
        // a Windows-style path does not lose its drive letter.
        let (file, key) = match lhs.rsplit_once(':') {
            Some((file, key)) if !file.is_empty() => (file.to_owned(), key.to_owned()),
            _ => (
                crate::pipeline::DEFAULT_PARAMS_FILE.to_owned(),
                lhs.to_owned(),
            ),
        };

        if key.is_empty() {
            return Err(ParamsError::MalformedOverride(text.to_owned()));
        }
        Ok(Self {
            file,
            key,
            value: scalar(raw),
        })
    }
}

/// Reads an override's value as YAML, so `8`, `0.25`, `true` and `[1, 2]` all
/// keep their type, and anything else stays a string.
fn scalar(raw: &str) -> Value {
    yaml_serde::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// Groups overrides by the file they apply to, preserving order within a file.
pub fn by_file(overrides: &[Override]) -> BTreeMap<&str, Vec<&Override>> {
    let mut grouped: BTreeMap<&str, Vec<&Override>> = BTreeMap::new();
    for o in overrides {
        grouped.entry(o.file.as_str()).or_default().push(o);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SAMPLE: &str = "train:\n  max_depth: 4\n  seed: 42\nprepare:\n  test_size: 0.25\n";

    #[test]
    fn reads_dotted_keys() {
        let p = Params::parse(SAMPLE, "params.yaml").unwrap();
        assert_eq!(p.get("train.max_depth"), Some(&json!(4)));
        assert_eq!(p.get("prepare.test_size"), Some(&json!(0.25)));
        assert_eq!(p.get("train.missing"), None);
        assert_eq!(p.get("nope.at.all"), None);
    }

    #[test]
    fn sets_an_existing_key_without_disturbing_the_rest() {
        let mut p = Params::parse(SAMPLE, "params.yaml").unwrap();
        p.set("train.max_depth", json!(8)).unwrap();

        assert_eq!(p.get("train.max_depth"), Some(&json!(8)));
        assert_eq!(p.get("train.seed"), Some(&json!(42)));
        assert_eq!(p.get("prepare.test_size"), Some(&json!(0.25)));
    }

    #[test]
    fn sets_a_key_whose_parents_do_not_exist_yet() {
        let mut p = Params::parse(SAMPLE, "params.yaml").unwrap();
        p.set("tune.search.trials", json!(30)).unwrap();
        assert_eq!(p.get("tune.search.trials"), Some(&json!(30)));
    }

    #[test]
    fn setting_through_a_scalar_is_refused() {
        let mut p = Params::parse(SAMPLE, "params.yaml").unwrap();
        assert!(p.set("train.seed.deeper", json!(1)).is_err());
    }

    #[test]
    fn a_written_file_reads_back_the_same() {
        let mut p = Params::parse(SAMPLE, "params.yaml").unwrap();
        p.set("train.max_depth", json!(8)).unwrap();

        let back = Params::parse(&p.to_yaml().unwrap(), "params.yaml").unwrap();
        assert_eq!(back.get("train.max_depth"), Some(&json!(8)));
        assert_eq!(back.get("train.seed"), Some(&json!(42)));
    }

    #[test]
    fn overrides_keep_their_type() {
        let parse = |s: &str| s.parse::<Override>().unwrap();
        assert_eq!(parse("train.max_depth=8").value, json!(8));
        assert_eq!(parse("prepare.test_size=0.25").value, json!(0.25));
        assert_eq!(parse("train.balanced=true").value, json!(true));
        assert_eq!(parse("train.name=wide forest").value, json!("wide forest"));
    }

    #[test]
    fn overrides_default_to_params_yaml_but_may_be_scoped() {
        let plain = "train.seed=1".parse::<Override>().unwrap();
        assert_eq!(plain.file, "params.yaml");
        assert_eq!(plain.key, "train.seed");

        let scoped = "tune.yaml:search.trials=30".parse::<Override>().unwrap();
        assert_eq!(scoped.file, "tune.yaml");
        assert_eq!(scoped.key, "search.trials");
    }

    #[test]
    fn a_malformed_override_is_rejected() {
        assert!("no-equals-sign".parse::<Override>().is_err());
        assert!("=8".parse::<Override>().is_err());
    }

    #[test]
    fn overrides_group_by_file() {
        let list: Vec<Override> = ["a.yaml:x=1", "b.yaml:y=2", "a.yaml:z=3"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let grouped = by_file(&list);
        assert_eq!(grouped["a.yaml"].len(), 2);
        assert_eq!(grouped["b.yaml"].len(), 1);
    }

    fn pipeline_with(params: &str) -> crate::pipeline::Pipeline {
        crate::pipeline::Pipeline::parse(
            &format!("stages:\n  train:\n    cmd: run\n    params:\n{params}"),
            "tsp.yaml",
        )
        .unwrap()
    }

    /// A working-tree read needs a Git only for the revision case, so these
    /// drive `read` with `rev: None` against a temp directory.
    fn read_workspace(root: &Path, pipeline: &crate::pipeline::Pipeline) -> Vec<Param> {
        let git = Git::discover(root).expect("a repository");
        read(&git, root, pipeline, None).unwrap()
    }

    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    /// The listing shows what staleness is decided from, and nothing else. A
    /// key no stage declares is not a parameter of this pipeline, however much
    /// it looks like one sitting in the same file.
    #[test]
    fn reads_only_the_keys_stages_declare() {
        let dir = repo_with(&[(
            "params.yaml",
            "train:\n  depth: 4\n  seed: 42\nunused:\n  thing: 9\n",
        )]);
        let root = dir.path().canonicalize().unwrap();
        let pipeline = pipeline_with("      - params.yaml:\n          - train.depth\n");

        let got = read_workspace(&root, &pipeline);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, "train.depth");
        assert_eq!(got[0].value, serde_json::json!(4));
    }

    #[test]
    fn a_key_missing_from_the_file_is_absent_rather_than_null() {
        let dir = repo_with(&[("params.yaml", "train:\n  depth: 4\n")]);
        let root = dir.path().canonicalize().unwrap();
        let pipeline = pipeline_with(
            "      - params.yaml:\n          - train.depth\n          - train.gone\n",
        );

        let got = read_workspace(&root, &pipeline);
        assert_eq!(got.len(), 1, "an absent key contributes no row: {got:?}");
    }

    /// Failing here would make a comparison against an old commit impossible
    /// because that commit happened to have a typo.
    #[test]
    fn an_unparseable_file_yields_nothing_rather_than_failing() {
        let dir = repo_with(&[("params.yaml", "train: [unclosed\n  : :\n")]);
        let root = dir.path().canonicalize().unwrap();
        let pipeline = pipeline_with("      - params.yaml:\n          - train.depth\n");

        assert!(read_workspace(&root, &pipeline).is_empty());
    }

    #[test]
    fn values_sort_by_file_then_key() {
        let dir = repo_with(&[("params.yaml", "b: 2\na: 1\n"), ("models.yaml", "z: 3\n")]);
        let root = dir.path().canonicalize().unwrap();
        let pipeline = pipeline_with(
            "      - params.yaml:\n          - b\n          - a\n      - models.yaml:\n          - z\n",
        );

        let got = read_workspace(&root, &pipeline);
        let names: Vec<String> = got
            .iter()
            .map(|p| format!("{}:{}", p.file, p.key))
            .collect();
        assert_eq!(names, ["models.yaml:z", "params.yaml:a", "params.yaml:b"]);
    }
}
