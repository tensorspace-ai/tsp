//! Reading and comparing metrics files.
//!
//! The flattening rules match Gitea's `services/ds/metrics.go` so a number
//! shown on the Data tab and the same number here read identically. JSON is
//! tried first because that is what training code usually writes.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::git::{Git, GitError};
use crate::pipeline::Pipeline;

type Result<T> = std::result::Result<T, GitError>;

/// One flattened scalar, keyed by the file it came from and its dotted path.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub file: String,
    pub key: String,
    pub value: Value,
}

impl Metric {
    /// The display form: integers stay integral and floats lose trailing zeros,
    /// so a table does not read as "0.850000".
    pub fn display(&self) -> String {
        match &self.value {
            Value::Number(n) => match n.as_f64() {
                Some(f) if f == f.trunc() && f.abs() < 1e15 => format!("{}", f as i64),
                Some(f) => {
                    let s = format!("{f:.6}");
                    s.trim_end_matches('0').trim_end_matches('.').to_owned()
                }
                None => n.to_string(),
            },
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        self.value.as_f64()
    }
}

/// Reads every metrics file the pipeline declares, at the working tree when
/// `rev` is `None` or at that revision otherwise.
pub fn read(
    git: &Git,
    root: &std::path::Path,
    pipeline: &Pipeline,
    rev: Option<&str>,
) -> Result<Vec<Metric>> {
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for stage in pipeline.stages.values() {
        for artifact in stage.metrics.iter() {
            if seen.contains(&artifact.path) {
                continue;
            }
            seen.push(artifact.path.clone());

            let raw: Option<Vec<u8>> = match rev {
                Some(rev) => git.read_blob_at(rev, &artifact.path)?,
                None => std::fs::read(root.join(&artifact.path)).ok(),
            };
            // A metrics file need not exist at every revision.
            let Some(raw) = raw else { continue };
            out.extend(parse(&artifact.path, &raw));
        }
    }

    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.key.cmp(&b.key)));
    Ok(out)
}

fn parse(file: &str, raw: &[u8]) -> Vec<Metric> {
    let text = String::from_utf8_lossy(raw);
    let document: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => match yaml_serde::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
    };

    let mut out = Vec::new();
    flatten(file, "", &document, 0, &mut out);
    out
}

/// Bounded so a deeply nested document cannot drive unbounded recursion.
const MAX_DEPTH: usize = 16;

fn flatten(file: &str, prefix: &str, value: &Value, depth: usize, out: &mut Vec<Metric>) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                flatten(file, &join(prefix, key), &map[key], depth + 1, out);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                flatten(file, &join(prefix, &i.to_string()), item, depth + 1, out);
            }
        }
        scalar => out.push(Metric {
            file: file.to_owned(),
            key: prefix.to_owned(),
            value: scalar.clone(),
        }),
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// A metric's direction, used to decide whether a delta is an improvement.
///
/// A rising number is progress for accuracy and a regression for loss, so the
/// two are told apart by name. Anything unrecognised stays unjudged: a wrong
/// verdict is worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    HigherIsBetter,
    LowerIsBetter,
    Unknown,
}

const LOWER_NAMES: [&str; 10] = [
    "loss",
    "error",
    "rmse",
    "mse",
    "mae",
    "mape",
    "perplexity",
    "latency",
    "duration",
    "cost",
];
const HIGHER_NAMES: [&str; 11] = [
    "accuracy",
    "acc",
    "f1",
    "precision",
    "recall",
    "auc",
    "iou",
    "dice",
    "bleu",
    "rouge",
    "r2",
];

/// Loss-like names are matched first: several of them contain a
/// higher-is-better name as a substring.
pub fn direction_of(key: &str) -> Direction {
    let key = key.to_lowercase();
    if LOWER_NAMES.iter().any(|n| key.contains(n)) {
        return Direction::LowerIsBetter;
    }
    if HIGHER_NAMES.iter().any(|n| key.contains(n)) {
        return Direction::HigherIsBetter;
    }
    Direction::Unknown
}

/// A metric paired across two sets.
pub struct Row {
    pub file: String,
    pub key: String,
    pub current: Option<String>,
    pub compare: Option<String>,
    pub delta: Option<f64>,
    pub improved: Option<bool>,
}

pub fn compare(current: &[Metric], other: &[Metric]) -> Vec<Row> {
    let index = |set: &[Metric]| -> BTreeMap<(String, String), Metric> {
        set.iter()
            .map(|m| ((m.file.clone(), m.key.clone()), m.clone()))
            .collect()
    };
    let (a, b) = (index(current), index(other));

    let mut keys: Vec<&(String, String)> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();

    keys.into_iter()
        .map(|k| {
            let (this, that) = (a.get(k), b.get(k));
            let delta = match (this.and_then(Metric::as_f64), that.and_then(Metric::as_f64)) {
                (Some(x), Some(y)) if x != y => Some(x - y),
                _ => None,
            };
            let improved = delta.and_then(|d| match direction_of(&k.1) {
                Direction::HigherIsBetter => Some(d > 0.0),
                Direction::LowerIsBetter => Some(d < 0.0),
                Direction::Unknown => None,
            });
            Row {
                file: k.0.clone(),
                key: k.1.clone(),
                current: this.map(Metric::display),
                compare: that.map(Metric::display),
                delta,
                improved,
            }
        })
        .collect()
}

/// Renders a delta without the binary floating-point noise a raw subtraction
/// exposes: 0.9631 - 0.9412 is 0.02189999999999992 in f64.
pub fn format_delta(delta: f64) -> String {
    let rounded = (delta * 1e6).round() / 1e6;
    let text = format!("{rounded:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if rounded > 0.0 {
        format!("+{text}")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_json() {
        let metrics = parse("m.json", br#"{"accuracy": 0.94, "loss": {"train": 0.1}}"#);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].key, "accuracy");
        assert_eq!(metrics[1].key, "loss.train");
    }

    #[test]
    fn falls_back_to_yaml() {
        let metrics = parse("m.yaml", b"accuracy: 0.94\nepochs: 12\n");
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].display(), "0.94");
        assert_eq!(metrics[1].display(), "12");
    }

    #[test]
    fn arrays_are_indexed() {
        let metrics = parse("m.json", br#"{"folds": [0.9, 0.95]}"#);
        assert_eq!(metrics[0].key, "folds.0");
        assert_eq!(metrics[1].key, "folds.1");
    }

    #[test]
    fn unparseable_content_yields_nothing() {
        assert!(parse("m.json", b"\x00not a document").is_empty());
    }

    #[test]
    fn display_trims_noise() {
        let metric = |v: Value| Metric {
            file: "m".into(),
            key: "k".into(),
            value: v,
        };
        assert_eq!(metric(serde_json::json!(12)).display(), "12");
        assert_eq!(metric(serde_json::json!(0.85)).display(), "0.85");
        assert_eq!(metric(serde_json::json!(0.9211)).display(), "0.9211");
    }

    #[test]
    fn direction_tells_loss_from_accuracy() {
        assert_eq!(direction_of("log_loss"), Direction::LowerIsBetter);
        assert_eq!(direction_of("val_accuracy"), Direction::HigherIsBetter);
        assert_eq!(direction_of("f1_macro"), Direction::HigherIsBetter);
        assert_eq!(direction_of("train.rmse"), Direction::LowerIsBetter);
        assert_eq!(direction_of("test_rows"), Direction::Unknown);
    }

    #[test]
    fn a_rising_loss_is_not_an_improvement() {
        let set = |v: f64| {
            vec![Metric {
                file: "m.json".into(),
                key: "log_loss".into(),
                value: serde_json::json!(v),
            }]
        };
        let rows = compare(&set(0.16), &set(0.12));
        assert_eq!(rows[0].improved, Some(false));

        let rows = compare(&set(0.12), &set(0.16));
        assert_eq!(rows[0].improved, Some(true));
    }

    #[test]
    fn an_unclassifiable_metric_is_not_judged() {
        let set = |v: f64| {
            vec![Metric {
                file: "m.json".into(),
                key: "test_rows".into(),
                value: serde_json::json!(v),
            }]
        };
        assert_eq!(compare(&set(38.0), &set(30.0))[0].improved, None);
    }

    #[test]
    fn format_delta_hides_representation_error() {
        assert_eq!(format_delta(0.9631 - 0.9412), "+0.0219");
        assert_eq!(format_delta(0.1231 - 0.1603), "-0.0372");
    }

    #[test]
    fn compare_keeps_keys_present_on_only_one_side() {
        let current = vec![Metric {
            file: "m.json".into(),
            key: "fresh".into(),
            value: serde_json::json!(1),
        }];
        let rows = compare(&current, &[]);
        assert_eq!(rows[0].current.as_deref(), Some("1"));
        assert_eq!(rows[0].compare, None);
    }
}
