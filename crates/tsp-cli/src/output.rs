//! What the commands emit, in both shapes they emit it in.
//!
//! The JSON here is a contract in the same sense the file formats are: it gets
//! consumed in CI, and moving a field breaks a pipeline nobody in this
//! repository can see. `docs/json.md` states the rules; the important ones are
//! that stdout carries the document and nothing else, and that an empty result
//! is an empty document rather than a sentence.
//!
//! The versioning rule is the *inverse* of `tsp.yaml`'s. A reader meeting an
//! unknown schema in a pipeline file refuses it whole, because someone else
//! wrote that file. Nobody but `tsp` writes these documents, so there is no
//! half-understood document to refuse: adding a field is safe, and only
//! removing, renaming, retyping or repurposing one is a bump.

use serde::Serialize;
use tsp_core::graph::Status;
use tsp_core::metrics;

/// The shape of the documents below. See the module comment for when it moves.
pub const SCHEMA: u32 = 1;

/// Whether a command writes a table for a person or a document for a program.
#[derive(Clone, Copy, PartialEq)]
pub enum Format {
    Text,
    Json,
}

impl Format {
    pub fn from_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Text }
    }

    pub fn is_json(self) -> bool {
        self == Self::Json
    }
}

/// Something the reader needs to know that is not part of the answer.
///
/// In a terminal these go to stderr; in a document they are a field, so a
/// consumer can see the same thing a person would have.
pub struct Note {
    pub code: &'static str,
    pub message: String,
}

/// The one note there is so far: a DVC repository whose stages all read as new.
pub const DVC_LOCK_NOT_READ: &str = "dvc_lock_not_read";

#[derive(Serialize)]
struct NoteOut<'a> {
    code: &'a str,
    message: &'a str,
}

fn notes_out(notes: &[Note]) -> Vec<NoteOut<'_>> {
    notes
        .iter()
        .map(|n| NoteOut {
            code: n.code,
            message: &n.message,
        })
        .collect()
}

/// Writes a document to stdout, with a trailing newline so a terminal and a
/// pipe both end cleanly.
pub fn emit(document: &impl Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(document)?);
    Ok(())
}

// ---------------------------------------------------------------- status ---

#[derive(Serialize)]
pub struct StatusDoc<'a> {
    schema: u32,
    kind: &'static str,
    pipeline_file: &'a str,
    stages: Vec<StatusStage<'a>>,
    summary: Summary,
    notes: Vec<NoteOut<'a>>,
}

#[derive(Serialize)]
struct StatusStage<'a> {
    name: &'a str,
    /// Exactly `Status::label()`, and `reason` exactly `Status::reason()`.
    /// These are the strings the conformance vectors pin and the second
    /// implementation is tested against; the document deliberately invents no
    /// vocabulary of its own.
    status: &'static str,
    reason: Option<&'a str>,
}

#[derive(Serialize)]
struct Summary {
    total: usize,
    current: usize,
    stale: usize,
    new: usize,
    unknown: usize,
    /// Precomputed because it is the number CI branches on, and deriving it
    /// wrongly is easy: `total - current` rather than `stale`, since a stage
    /// that is new or undecidable also needs a run.
    needs_run: usize,
}

pub fn status_document<'a>(
    pipeline_file: &'a str,
    statuses: &'a [(String, Status)],
    notes: &'a [Note],
) -> StatusDoc<'a> {
    let mut summary = Summary {
        total: statuses.len(),
        current: 0,
        stale: 0,
        new: 0,
        unknown: 0,
        needs_run: 0,
    };
    let stages = statuses
        .iter()
        .map(|(name, status)| {
            match status {
                Status::Current => summary.current += 1,
                Status::Stale(_) => summary.stale += 1,
                Status::New => summary.new += 1,
                Status::Unknown(_) => summary.unknown += 1,
            }
            if status.needs_run() {
                summary.needs_run += 1;
            }
            StatusStage {
                name,
                status: status.label(),
                reason: status.reason(),
            }
        })
        .collect();

    StatusDoc {
        schema: SCHEMA,
        kind: "status",
        pipeline_file,
        stages,
        summary,
        notes: notes_out(notes),
    }
}

// ------------------------------------------------------- values and rows ---

#[derive(Serialize)]
pub struct ValuesDoc<'a> {
    schema: u32,
    kind: &'static str,
    values: Vec<ValueOut<'a>>,
    notes: Vec<NoteOut<'a>>,
}

#[derive(Serialize)]
struct ValueOut<'a> {
    file: &'a str,
    key: &'a str,
    /// The raw scalar, so a consumer can do arithmetic, *and* the rendering, so
    /// one drawing a table matches this CLI byte for byte. Dropping either
    /// forces a reimplementation of the other.
    value: &'a serde_json::Value,
    display: String,
}

pub fn values_document<'a>(
    kind: &'static str,
    values: &'a [metrics::Metric],
    notes: &'a [Note],
) -> ValuesDoc<'a> {
    ValuesDoc {
        schema: SCHEMA,
        kind,
        values: values
            .iter()
            .map(|m| ValueOut {
                file: &m.file,
                key: &m.key,
                value: &m.value,
                display: m.display(),
            })
            .collect(),
        notes: notes_out(notes),
    }
}

#[derive(Serialize)]
pub struct ComparisonDoc<'a> {
    schema: u32,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    experiment: Option<ExperimentOut<'a>>,
    current_label: &'a str,
    compare_label: &'a str,
    rows: Vec<RowOut<'a>>,
    notes: Vec<NoteOut<'a>>,
}

#[derive(Serialize)]
pub struct ExperimentOut<'a> {
    pub name: &'a str,
    pub commit: &'a str,
}

#[derive(Serialize)]
struct RowOut<'a> {
    file: &'a str,
    key: &'a str,
    /// Display strings, or null when the side has no value. Never `"-"`: the
    /// dash is a table artefact and must not leak into a document.
    current: Option<&'a str>,
    compare: Option<&'a str>,
    delta: Option<f64>,
    delta_display: Option<String>,
    /// Omitted entirely for parameters rather than nulled. metrics::compare
    /// judges a row whenever the key's name implies a direction, so a parameter
    /// named for a loss *will* come back judged; having no field to put it in
    /// is what makes publishing that judgement structurally impossible.
    #[serde(skip_serializing_if = "Option::is_none")]
    improved: Option<Judged>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<&'static str>,
}

/// `true`, `false`, or "the name settles no direction" — three states, and the
/// third must not collapse into the second.
#[derive(Serialize)]
#[serde(untagged)]
enum Judged {
    Known(bool),
    #[serde(serialize_with = "serialize_null")]
    Unknown,
}

fn serialize_null<S: serde::Serializer>(serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_none()
}

fn direction_name(direction: metrics::Direction) -> &'static str {
    match direction {
        metrics::Direction::HigherIsBetter => "higher_is_better",
        metrics::Direction::LowerIsBetter => "lower_is_better",
        metrics::Direction::Unknown => "unknown",
    }
}

/// Builds a comparison document. `judged` is false for parameters, where
/// "better" has no meaning.
pub fn comparison_document<'a>(
    kind: &'static str,
    rows: &'a [metrics::Row],
    current_label: &'a str,
    compare_label: &'a str,
    judged: bool,
    experiment: Option<ExperimentOut<'a>>,
    notes: &'a [Note],
) -> ComparisonDoc<'a> {
    ComparisonDoc {
        schema: SCHEMA,
        kind,
        experiment,
        current_label,
        compare_label,
        rows: rows
            .iter()
            .map(|row| RowOut {
                file: &row.file,
                key: &row.key,
                current: row.current.as_deref(),
                compare: row.compare.as_deref(),
                delta: row.delta,
                delta_display: row.delta.map(metrics::format_delta),
                improved: judged.then_some(match row.improved {
                    Some(verdict) => Judged::Known(verdict),
                    None => Judged::Unknown,
                }),
                direction: judged.then(|| direction_name(metrics::direction_of(&row.key))),
            })
            .collect(),
        notes: notes_out(notes),
    }
}

// -------------------------------------------------------------- exp list ---

#[derive(Serialize)]
pub struct ExpListDoc<'a> {
    schema: u32,
    kind: &'static str,
    /// The column order, so a consumer reproduces the table exactly. Each entry
    /// carries its file as well as its key, because two metrics files may use
    /// the same dotted key for different measurements.
    keys: &'a [ColumnKey],
    rows: Vec<ExpRowOut<'a>>,
}

#[derive(Serialize)]
struct ExpRowOut<'a> {
    name: &'a str,
    baseline: bool,
    /// Positional, matching `keys`. A map keyed by name alone could not
    /// represent two files sharing a dotted key, which is the collision this
    /// table now distinguishes. A null cell is the one the table draws as a
    /// dash.
    metrics: Vec<Option<CellOut<'a>>>,
}

#[derive(Serialize)]
struct CellOut<'a> {
    value: &'a serde_json::Value,
    display: String,
}

pub fn exp_list_document<'a>(
    keys: &'a [ColumnKey],
    read: &'a [(String, Vec<metrics::Metric>)],
) -> ExpListDoc<'a> {
    ExpListDoc {
        schema: SCHEMA,
        kind: "exp_list",
        keys,
        rows: read
            .iter()
            .enumerate()
            .map(|(i, (name, produced))| ExpRowOut {
                name,
                baseline: i == 0,
                metrics: keys
                    .iter()
                    .map(|column| {
                        produced
                            .iter()
                            .find(|m| m.file == column.file && m.key == column.key)
                            .map(|m| CellOut {
                                value: &m.value,
                                display: m.display(),
                            })
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ----------------------------------------------------------------- text ----

/// One column of the experiments table: a metrics file and a key within it.
///
/// Both, because two metrics files may carry the same dotted key and they are
/// two different measurements. Matching on the key alone silently reported one
/// of them under the other's column.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ColumnKey {
    pub file: String,
    pub key: String,
}

/// Column headings: the metric's name, qualified by its file only where two
/// columns would otherwise read alike. Qualifying every heading would widen the
/// common table for a collision almost no pipeline has.
pub fn headings(keys: &[ColumnKey]) -> Vec<String> {
    keys.iter()
        .map(|column| {
            if keys.iter().filter(|other| other.key == column.key).count() > 1 {
                format!("{}:{}", column.file, column.key)
            } else {
                column.key.clone()
            }
        })
        .collect()
}

/// A metric value already rendered for the table, or "-" when absent.
pub struct Metricish(pub String);

pub fn values_for(keys: &[ColumnKey], metrics: &[metrics::Metric]) -> Vec<Metricish> {
    keys.iter()
        .map(|column| {
            Metricish(
                metrics
                    .iter()
                    .find(|m| m.file == column.file && m.key == column.key)
                    .map_or_else(|| "-".to_owned(), |m| m.display()),
            )
        })
        .collect()
}

pub fn print_values(values: &[metrics::Metric]) {
    let width = values.iter().map(|m| m.key.len()).max().unwrap_or(0);
    for value in values {
        println!("  {:<width$}  {}", value.key, value.display());
    }
}

pub fn print_comparison(rows: &[metrics::Row], current_label: &str, compare_label: &str) {
    let key_width = rows
        .iter()
        .map(|r| r.key.len())
        .chain([6])
        .max()
        .unwrap_or(6);
    println!(
        "  {:<key_width$}  {:>12}  {:>12}  DELTA",
        "METRIC", current_label, compare_label
    );

    for row in rows {
        let delta = match (row.delta, row.improved) {
            (Some(d), Some(true)) => format!("{} better", metrics::format_delta(d)),
            (Some(d), Some(false)) => format!("{} worse", metrics::format_delta(d)),
            (Some(d), None) => metrics::format_delta(d),
            (None, _) => String::new(),
        };
        // Trimmed: a row with no delta would otherwise end in the padding the
        // column left behind.
        println!(
            "{}",
            format!(
                "  {:<key_width$}  {:>12}  {:>12}  {delta}",
                row.key,
                row.current.as_deref().unwrap_or("-"),
                row.compare.as_deref().unwrap_or("-"),
            )
            .trim_end()
        );
    }
}

/// A parameter comparison: the metrics table without the verdict column.
///
/// metrics::compare judges a row whenever the key's name implies a direction,
/// and it matches on substrings, so a parameter called `train.loss_weight`
/// comes back "better" for having gone down. A parameter is a setting rather
/// than a result; there is nothing for it to be better at.
pub fn print_params_comparison(rows: &[metrics::Row], current_label: &str, compare_label: &str) {
    let key_width = rows
        .iter()
        .map(|r| r.key.len())
        .chain([9])
        .max()
        .unwrap_or(9);
    println!(
        "  {:<key_width$}  {:>12}  {:>12}  DELTA",
        "PARAMETER", current_label, compare_label
    );

    for row in rows {
        let delta = row.delta.map(metrics::format_delta).unwrap_or_default();
        println!(
            "{}",
            format!(
                "  {:<key_width$}  {:>12}  {:>12}  {delta}",
                row.key,
                row.current.as_deref().unwrap_or("-"),
                row.compare.as_deref().unwrap_or("-"),
            )
            .trim_end()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(file: &str, key: &str, value: serde_json::Value) -> metrics::Metric {
        metrics::Metric {
            file: file.to_owned(),
            key: key.to_owned(),
            value,
        }
    }

    fn json_of(document: &impl Serialize) -> serde_json::Value {
        serde_json::to_value(document).unwrap()
    }

    #[test]
    fn the_document_names_its_schema_and_kind() {
        let values = [metric("metrics.json", "accuracy", serde_json::json!(1))];
        let doc = json_of(&values_document("metrics", &values, &[]));

        assert_eq!(doc["schema"], SCHEMA);
        assert_eq!(doc["kind"], "metrics");
        assert_eq!(doc["values"][0]["value"], 1);
        assert_eq!(doc["values"][0]["display"], "1");
    }

    /// The dash is how a table draws a missing side. A document says null, or a
    /// consumer ends up comparing against the string "-".
    #[test]
    fn a_missing_side_is_null_rather_than_a_dash() {
        let rows = [metrics::Row {
            file: "metrics.json".to_owned(),
            key: "accuracy".to_owned(),
            current: Some("12".to_owned()),
            compare: None,
            delta: None,
            improved: None,
        }];
        let doc = json_of(&comparison_document(
            "metrics",
            &rows,
            "workspace",
            "HEAD",
            true,
            None,
            &[],
        ));

        assert_eq!(doc["rows"][0]["current"], "12");
        assert!(doc["rows"][0]["compare"].is_null());
        assert!(doc["rows"][0]["delta"].is_null());
    }

    /// Three states, not two: a metric whose name settles no direction is
    /// unjudged, which is not the same as judged worse.
    #[test]
    fn an_unjudgeable_metric_is_null_rather_than_false() {
        let rows = [metrics::Row {
            file: "metrics.json".to_owned(),
            key: "lines".to_owned(),
            current: Some("6".to_owned()),
            compare: Some("3".to_owned()),
            delta: Some(3.0),
            improved: None,
        }];
        let doc = json_of(&comparison_document(
            "metrics",
            &rows,
            "workspace",
            "HEAD",
            true,
            None,
            &[],
        ));

        assert!(doc["rows"][0]["improved"].is_null());
        assert_eq!(doc["rows"][0]["direction"], "unknown");
        assert_eq!(doc["rows"][0]["delta_display"], "+3");
    }

    /// The field is absent, not null. metrics::compare will happily judge
    /// `train.loss_weight`; having nowhere to put the verdict is what stops it
    /// being published.
    #[test]
    fn an_unjudged_comparison_omits_the_verdict_fields() {
        let rows = [metrics::Row {
            file: "params.yaml".to_owned(),
            key: "train.loss_weight".to_owned(),
            current: Some("1".to_owned()),
            compare: Some("2".to_owned()),
            delta: Some(-1.0),
            improved: Some(true),
        }];
        let doc = json_of(&comparison_document(
            "params",
            &rows,
            "workspace",
            "HEAD",
            false,
            None,
            &[],
        ));

        let row = doc["rows"][0].as_object().unwrap();
        assert!(!row.contains_key("improved"), "{row:?}");
        assert!(!row.contains_key("direction"), "{row:?}");
        assert_eq!(row["delta_display"], "-1", "the delta is still reported");
    }

    #[test]
    fn the_status_summary_counts_every_stage_that_needs_a_run() {
        let statuses = [
            ("a".to_owned(), Status::Current),
            ("b".to_owned(), Status::Stale("x moved".to_owned())),
            ("c".to_owned(), Status::New),
            ("d".to_owned(), Status::Unknown("no such path".to_owned())),
        ];
        let doc = json_of(&status_document("tsp.yaml", &statuses, &[]));

        assert_eq!(doc["summary"]["total"], 4);
        assert_eq!(doc["summary"]["current"], 1);
        assert_eq!(doc["summary"]["needs_run"], 3, "new and unknown count too");
        assert!(
            doc["stages"][0]["reason"].is_null(),
            "current has no reason"
        );
        assert_eq!(doc["stages"][2]["reason"], "never run");
    }
}
