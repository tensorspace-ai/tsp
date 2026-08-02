//! Parameter files and the overrides an experiment applies to them.
//!
//! Values are held as JSON rather than a YAML-specific type: the lock records
//! them, metrics files are usually JSON, and keeping one scalar model across
//! both avoids a second set of number-formatting rules.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

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
}
