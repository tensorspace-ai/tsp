//! Reading the tabular data behind a plot.
//!
//! A plot data file is a list of records: rows of a CSV, or objects in a JSON
//! or YAML array. Everything downstream — axis fields, series, confusion cells —
//! is a projection of that list, which keeps one loader for four formats.

use indexmap::IndexMap;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("cannot parse {path}: {detail}")]
    Malformed { path: String, detail: String },
    #[error("{path} holds no list of records to plot")]
    NoRecords { path: String },
}

type Result<T> = std::result::Result<T, DataError>;

/// One record. Ordered so a file's own column order survives into the legend
/// and the "first numeric field" fallback is predictable.
pub type Row = IndexMap<String, Value>;

/// Reads a data file, choosing the parser by extension.
///
/// `header` applies to delimited formats only, and is true for all but a file
/// the pipeline explicitly declared headerless.
pub fn parse_with_header(path: &str, bytes: &[u8], header: bool) -> Result<Vec<Row>> {
    let text = String::from_utf8_lossy(bytes);
    match extension(path) {
        "csv" => delimited(path, &text, ',', header),
        "tsv" => delimited(path, &text, '\t', header),
        "yaml" | "yml" => structured(path, yaml_value(&text, path)?),
        // JSON is the default: it is what training code writes, and a `.json`
        // document also parses as YAML if the extension is missing or odd.
        _ => {
            let value = serde_json::from_str(&text)
                .or_else(|_| yaml_value(&text, path).map_err(|_| ()))
                .map_err(|()| DataError::Malformed {
                    path: path.to_owned(),
                    detail: "not valid JSON or YAML".to_owned(),
                })?;
            structured(path, value)
        }
    }
}

/// Reads a data file that has a header, which is all but the declared exception.
pub fn parse(path: &str, bytes: &[u8]) -> Result<Vec<Row>> {
    parse_with_header(path, bytes, true)
}

/// The field name DVC gives the row index when a plot names no x axis.
pub const INDEX_FIELD: &str = "step";

fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, ext)| ext)
}

fn yaml_value(text: &str, path: &str) -> Result<Value> {
    yaml_serde::from_str(text).map_err(|e| DataError::Malformed {
        path: path.to_owned(),
        detail: e.to_string(),
    })
}

/// Projects a decoded document into rows, matching DVC's rule.
///
/// Every list in the document is considered, at any depth — a bare array, or
/// `{"train": [...]}`, or several such keys. A list qualifies when its items
/// are all objects sharing one key set, and qualifying lists are merged
/// index-wise, so a file that records train and eval side by side yields one
/// row per step carrying both.
fn structured(path: &str, value: Value) -> Result<Vec<Row>> {
    let mut candidates = Vec::new();
    collect_record_lists(&value, &mut candidates);
    if candidates.is_empty() {
        return Err(DataError::NoRecords {
            path: path.to_owned(),
        });
    }

    let len = candidates.iter().map(Vec::len).max().unwrap_or(0);
    Ok((0..len)
        .map(|i| {
            let mut row = Row::new();
            for list in &candidates {
                if let Some(item) = list.get(i) {
                    row.extend(item.clone());
                }
            }
            row
        })
        .collect())
}

/// Gathers every list of consistently-shaped objects, depth first.
fn collect_record_lists(value: &Value, out: &mut Vec<Vec<Row>>) {
    match value {
        Value::Array(items) => {
            if let Some(rows) = as_records(items) {
                out.push(rows);
            }
            // A list that is not itself records may still hold some.
            for item in items {
                collect_record_lists(item, out);
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                collect_record_lists(child, out);
            }
        }
        _ => {}
    }
}

/// A list qualifies only when every item is an object with the same keys —
/// DVC's consistency rule, which is what stops a list of nested config blobs
/// being mistaken for datapoints.
fn as_records(items: &[Value]) -> Option<Vec<Row>> {
    let first = items.first()?.as_object()?;
    let keys: Vec<&String> = first.keys().collect();

    for item in items {
        let object = item.as_object()?;
        if object.len() != keys.len() || !keys.iter().all(|k| object.contains_key(*k)) {
            return None;
        }
    }

    Some(
        items
            .iter()
            .map(|item| {
                item.as_object()
                    .expect("checked above")
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .collect(),
    )
}

/// Parses delimited text, taking the first line as the header.
///
/// Deliberately not a CSV library: plot data is written by the pipeline being
/// plotted, so it is machine-generated and simple. Quoted fields are honoured
/// because a label containing a comma is the one case that does show up.
///
/// With `header` false the columns are named `"0"`, `"1"`, … as DVC's
/// `--no-header` does, so a headerless file can still name its axes.
fn delimited(path: &str, text: &str, sep: char, header: bool) -> Result<Vec<Row>> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty()).peekable();
    let fields: Vec<String> = match lines.peek() {
        None => {
            return Err(DataError::NoRecords {
                path: path.to_owned(),
            });
        }
        Some(first) if header => {
            let names = split_line(first, sep);
            lines.next();
            names
        }
        Some(first) => (0..split_line(first, sep).len())
            .map(|i| i.to_string())
            .collect(),
    };

    Ok(lines
        .map(|line| {
            split_line(line, sep)
                .into_iter()
                .enumerate()
                .map(|(i, cell)| {
                    let name = fields.get(i).cloned().unwrap_or_else(|| i.to_string());
                    (name, scalar(&cell))
                })
                .collect()
        })
        .collect())
}

fn split_line(line: &str, sep: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells.into_iter().map(|c| c.trim().to_owned()).collect()
}

/// Reads a cell as a number when it is one, so axes stay numeric.
fn scalar(cell: &str) -> Value {
    if let Ok(n) = cell.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(f) = cell.parse::<f64>()
        && f.is_finite()
    {
        return Value::from(f);
    }
    Value::String(cell.to_owned())
}

/// The fields every row shares, in first-seen order.
pub fn fields(rows: &[Row]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for row in rows {
        for key in row.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
    }
    names
}

/// The default y field: the last one of the first record.
///
/// DVC's rule, and a deliberate one — a results file usually ends with the
/// number it was written to record, after the identifiers that locate it.
pub fn default_y_field(rows: &[Row]) -> Option<String> {
    rows.first()?.keys().next_back().cloned()
}

/// Writes the row index into each record under `step`.
///
/// Matches DVC, including that it overwrites a `step` the data already had: a
/// plot that means to use its own must name `x: step` explicitly.
pub fn add_index(rows: &mut [Row]) {
    for (i, row) in rows.iter_mut().enumerate() {
        row.insert(INDEX_FIELD.to_owned(), Value::from(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_json_array_of_objects() {
        let rows = parse(
            "roc.json",
            br#"[{"fpr": 0.0, "tpr": 0.1}, {"fpr": 0.5, "tpr": 0.9}]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["fpr"], json!(0.0));
        assert_eq!(rows[1]["tpr"], json!(0.9));
    }

    /// A script that writes one file per result usually nests the list.
    #[test]
    fn finds_a_list_nested_under_a_key() {
        let rows = parse("out.json", br#"{"roc": [{"fpr": 0.2}]}"#).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["fpr"], json!(0.2));
    }

    /// Several lists merge index-wise, which is how a file recording train and
    /// eval side by side yields one row per step.
    #[test]
    fn merges_several_lists_by_index() {
        let rows = parse(
            "out.json",
            br#"{"train": [{"loss": 0.9}, {"loss": 0.5}], "eval": [{"acc": 0.4}, {"acc": 0.8}]}"#,
        )
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["loss"], json!(0.9));
        assert_eq!(rows[0]["acc"], json!(0.4));
        assert_eq!(rows[1]["acc"], json!(0.8));
    }

    /// Items must share a key set, so a list of unrelated blobs is not data.
    #[test]
    fn a_list_of_inconsistent_objects_is_not_records() {
        assert!(matches!(
            parse("out.json", br#"{"cfg": [{"a": 1}, {"b": 2}]}"#),
            Err(DataError::NoRecords { .. })
        ));
    }

    #[test]
    fn reads_yaml() {
        let rows = parse(
            "p.yaml",
            b"- step: 1\n  loss: 0.5\n- step: 2\n  loss: 0.3\n",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["loss"], json!(0.3));
    }

    #[test]
    fn reads_csv_with_a_header() {
        let rows = parse("p.csv", b"epoch,loss\n1,0.5\n2,0.25\n").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["epoch"], json!(1));
        assert_eq!(rows[1]["loss"], json!(0.25));
    }

    #[test]
    fn reads_tsv() {
        let rows = parse("p.tsv", b"a\tb\n1\t2\n").unwrap();
        assert_eq!(rows[0]["a"], json!(1));
        assert_eq!(rows[0]["b"], json!(2));
    }

    /// A label containing the separator is the one quoting case that occurs.
    #[test]
    fn honours_quoted_cells() {
        let rows = parse("p.csv", b"name,value\n\"setosa, iris\",3\n").unwrap();
        assert_eq!(rows[0]["name"], json!("setosa, iris"));
        assert_eq!(rows[0]["value"], json!(3));
    }

    #[test]
    fn keeps_non_numeric_cells_as_text() {
        let rows = parse("p.csv", b"actual,count\nsetosa,12\n").unwrap();
        assert_eq!(rows[0]["actual"], json!("setosa"));
        assert_eq!(rows[0]["count"], json!(12));
    }

    /// Records are objects; a list of bare numbers names no fields to plot.
    #[test]
    fn a_list_of_scalars_is_not_records() {
        assert!(matches!(
            parse("p.json", b"[0.9, 0.8, 0.7]"),
            Err(DataError::NoRecords { .. })
        ));
    }

    #[test]
    fn a_document_with_no_list_is_reported() {
        assert!(matches!(
            parse("p.json", br#"{"accuracy": 0.9}"#),
            Err(DataError::NoRecords { .. })
        ));
    }

    #[test]
    fn a_headerless_file_gets_positional_field_names() {
        let rows = parse_with_header("p.csv", b"1,0.5\n2,0.25\n", false).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["0"], json!(1));
        assert_eq!(rows[1]["1"], json!(0.25));
    }

    #[test]
    fn the_default_y_field_is_the_last_one() {
        let rows = parse("p.csv", b"epoch,train_loss,val_loss\n1,0.5,0.6\n").unwrap();
        assert_eq!(default_y_field(&rows).as_deref(), Some("val_loss"));
    }

    /// The index overwrites any `step` the data carried, as DVC's does.
    #[test]
    fn add_index_numbers_the_rows() {
        let mut rows = parse(
            "p.json",
            br#"[{"step": 7, "loss": 1}, {"step": 9, "loss": 2}]"#,
        )
        .unwrap();
        add_index(&mut rows);
        assert_eq!(rows[0][INDEX_FIELD], json!(0));
        assert_eq!(rows[1][INDEX_FIELD], json!(1));
    }

    #[test]
    fn malformed_input_is_reported_with_its_path() {
        let err = parse("p.json", b"{{{").unwrap_err();
        assert!(err.to_string().contains("p.json"), "{err}");
    }

    /// Field order follows the file, not the alphabet — the legend and the
    /// default-y rule both read it.
    #[test]
    fn fields_keep_the_files_own_order() {
        let rows = parse("p.json", br#"[{"b": 1, "a": 2}, {"b": 3, "a": 4}]"#).unwrap();
        assert_eq!(fields(&rows), ["b", "a"]);
    }
}
